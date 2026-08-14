use saphyr_parser::{Event, Parser, ScalarStyle, Span};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

const API_VERSION: &str = "termux-stacks/v1alpha1";
const MAX_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 16;
const MAX_NODES: usize = 1024;
const MAX_COLLECTION_ITEMS: usize = 128;
const MAX_SCALAR_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Manifest {
    pub(crate) name: String,
    pub(crate) service: Service,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Service {
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) command: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    Io,
    Invalid,
    Unsupported,
}

impl ErrorKind {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Invalid => "invalid_manifest",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Error {
    kind: ErrorKind,
    message: String,
    location: Option<Location>,
}

impl Error {
    pub(crate) fn kind(&self) -> ErrorKind {
        self.kind
    }

    fn invalid(message: impl Into<String>, location: Option<Location>) -> Self {
        Self {
            kind: ErrorKind::Invalid,
            message: message.into(),
            location,
        }
    }

    fn unsupported(message: impl Into<String>, location: Option<Location>) -> Self {
        Self {
            kind: ErrorKind::Unsupported,
            message: message.into(),
            location,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(location) = self.location {
            write!(
                formatter,
                "{} at line {}, column {}",
                self.message, location.line, location.column
            )
        } else {
            formatter.write_str(&self.message)
        }
    }
}

pub(crate) fn load(path: &Path) -> Result<(Manifest, String), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|error| Error {
        kind: ErrorKind::Io,
        message: format!("cannot inspect manifest {}: {error}", path.display()),
        location: None,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error {
            kind: ErrorKind::Io,
            message: format!(
                "manifest is not a regular non-symlink file: {}",
                path.display()
            ),
            location: None,
        });
    }
    if metadata.len() > MAX_BYTES as u64 {
        return Err(Error::invalid(
            format!("manifest exceeds the {MAX_BYTES}-byte limit"),
            None,
        ));
    }

    let text = fs::read_to_string(path).map_err(|error| Error {
        kind: ErrorKind::Io,
        message: format!("cannot read manifest {} as UTF-8: {error}", path.display()),
        location: None,
    })?;
    let manifest = parse(&text)?;
    Ok((manifest, text))
}

pub(crate) fn parse(input: &str) -> Result<Manifest, Error> {
    if input.len() > MAX_BYTES {
        return Err(Error::invalid(
            format!("manifest exceeds the {MAX_BYTES}-byte limit"),
            None,
        ));
    }

    let mut events = Vec::new();
    for parsed in Parser::new_from_str(input) {
        let (event, span) = parsed.map_err(|error| Error::invalid(error.to_string(), None))?;
        events.push((event, span));
        if events.len() > MAX_NODES * 4 {
            return Err(Error::invalid("manifest has too many parser events", None));
        }
    }

    let mut cursor = Cursor::new(events);
    cursor.expect_stream_start()?;
    cursor.expect_document_start()?;
    let root = cursor.node(0)?;
    cursor.expect_document_end()?;
    cursor.expect_stream_end()?;
    validate(root)
}

fn validate(root: Node) -> Result<Manifest, Error> {
    let mut top = root.into_mapping("manifest")?;
    reject_unknown_or_unsupported(
        &top,
        "manifest",
        &["apiVersion", "kind", "metadata", "services"],
        &["volumes"],
    )?;

    let api_version =
        take_required(&mut top, "apiVersion", "apiVersion")?.into_string("apiVersion")?;
    if api_version != API_VERSION {
        return Err(Error::invalid(
            format!("apiVersion must be {API_VERSION:?}"),
            None,
        ));
    }

    let kind = take_required(&mut top, "kind", "kind")?.into_string("kind")?;
    if kind != "Stack" {
        return Err(Error::invalid("kind must be \"Stack\"", None));
    }

    let metadata_node = take_required(&mut top, "metadata", "metadata")?;
    let mut metadata = metadata_node.into_mapping("metadata")?;
    reject_unknown_or_unsupported(&metadata, "metadata", &["name"], &[])?;
    let name_node = take_required(&mut metadata, "name", "metadata.name")?;
    let name_location = name_node.location();
    let name = name_node.into_string("metadata.name")?;
    validate_name(&name, "metadata.name", Some(name_location))?;

    let services_node = take_required(&mut top, "services", "services")?;
    let services_location = services_node.location();
    let mut services = services_node.into_mapping("services")?;
    if services.len() != 1 {
        return Err(Error::unsupported(
            "the vertical slice supports exactly one service",
            Some(services_location),
        ));
    }

    let (service_name, service_entry) = services.pop_first().expect("one service checked");
    validate_name(
        &service_name,
        "service name",
        Some(service_entry.key_location),
    )?;
    let service_path = format!("services.{service_name}");
    let mut service = service_entry.value.into_mapping(&service_path)?;
    reject_unknown_or_unsupported(
        &service,
        &service_path,
        &["image", "command"],
        &["environment", "mounts", "ports", "dependsOn", "restart"],
    )?;

    let image = take_required(&mut service, "image", &format!("{service_path}.image"))?
        .into_string(&format!("{service_path}.image"))?;
    if image.is_empty()
        || image.len() > 2048
        || image.starts_with('-')
        || image.chars().any(char::is_control)
    {
        return Err(Error::invalid(
            format!(
                "{service_path}.image must be 1..=2048 characters, contain no control characters, and not start with '-'"
            ),
            None,
        ));
    }

    let command = match service.remove("command") {
        None => None,
        Some(entry) => {
            let command_path = format!("{service_path}.command");
            let values = entry.value.into_sequence(&command_path)?;
            if values.is_empty() {
                return Err(Error::invalid(
                    format!("{command_path} must contain at least one argument"),
                    Some(entry.key_location),
                ));
            }
            let mut command = Vec::with_capacity(values.len());
            for (index, value) in values.into_iter().enumerate() {
                command.push(value.into_string(&format!("{command_path}[{index}]"))?);
            }
            if command[0].is_empty() {
                return Err(Error::invalid(
                    format!("{command_path}[0] must not be empty"),
                    Some(entry.key_location),
                ));
            }
            Some(command)
        }
    };

    Ok(Manifest {
        name,
        service: Service {
            name: service_name,
            image,
            command,
        },
    })
}

fn validate_name(name: &str, path: &str, location: Option<Location>) -> Result<(), Error> {
    let bytes = name.as_bytes();
    let valid = (1..=48).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !name.starts_with("termux-stacks-");
    if valid {
        Ok(())
    } else {
        Err(Error::invalid(
            format!(
                "{path} must match ^[a-z][a-z0-9-]{{0,47}}$ and must not start with termux-stacks-"
            ),
            location,
        ))
    }
}

pub(crate) fn validate_stack_name(name: &str) -> Result<(), Error> {
    validate_name(name, "stack name", None)
}

fn take_required(
    mapping: &mut BTreeMap<String, MappingEntry>,
    key: &str,
    path: &str,
) -> Result<Node, Error> {
    mapping
        .remove(key)
        .map(|entry| entry.value)
        .ok_or_else(|| Error::invalid(format!("missing required field {path}"), None))
}

fn reject_unknown_or_unsupported(
    mapping: &BTreeMap<String, MappingEntry>,
    path: &str,
    allowed: &[&str],
    unsupported: &[&str],
) -> Result<(), Error> {
    for (key, entry) in mapping {
        if allowed.contains(&key.as_str()) {
            continue;
        }
        let field_path = if path == "manifest" {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if unsupported.contains(&key.as_str()) {
            return Err(Error::unsupported(
                format!("field {field_path} is not implemented in the vertical slice"),
                Some(entry.key_location),
            ));
        }
        return Err(Error::invalid(
            format!("unknown field {field_path}"),
            Some(entry.key_location),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct Location {
    line: usize,
    column: usize,
}

impl From<Span> for Location {
    fn from(span: Span) -> Self {
        Self {
            line: span.start.line(),
            column: span.start.col(),
        }
    }
}

#[derive(Debug)]
enum Node {
    Scalar(Scalar),
    Sequence(Vec<Node>, Location),
    Mapping(BTreeMap<String, MappingEntry>, Location),
}

impl Node {
    fn location(&self) -> Location {
        match self {
            Self::Scalar(value) => value.location,
            Self::Sequence(_, location) | Self::Mapping(_, location) => *location,
        }
    }

    fn into_string(self, path: &str) -> Result<String, Error> {
        match self {
            Self::Scalar(value) if value.is_string() => Ok(value.value),
            Self::Scalar(value) => Err(Error::invalid(
                format!("{path} must be a string"),
                Some(value.location),
            )),
            other => Err(Error::invalid(
                format!("{path} must be a string"),
                Some(other.location()),
            )),
        }
    }

    fn into_sequence(self, path: &str) -> Result<Vec<Node>, Error> {
        match self {
            Self::Sequence(values, _) => Ok(values),
            other => Err(Error::invalid(
                format!("{path} must be an array"),
                Some(other.location()),
            )),
        }
    }

    fn into_mapping(self, path: &str) -> Result<BTreeMap<String, MappingEntry>, Error> {
        match self {
            Self::Mapping(values, _) => Ok(values),
            other => Err(Error::invalid(
                format!("{path} must be a mapping"),
                Some(other.location()),
            )),
        }
    }
}

#[derive(Debug)]
struct MappingEntry {
    key_location: Location,
    value: Node,
}

#[derive(Debug)]
struct Scalar {
    value: String,
    style: ScalarStyle,
    location: Location,
}

impl Scalar {
    fn is_string(&self) -> bool {
        self.style != ScalarStyle::Plain || !is_plain_core_literal(&self.value)
    }
}

fn is_plain_core_literal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "~" | "null" | "true" | "false" | ".nan" | ".inf" | "+.inf" | "-.inf"
    ) {
        return true;
    }
    if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
        return true;
    }
    if let Some(hex) = lower.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if let Some(octal) = lower.strip_prefix("0o") {
        return !octal.is_empty() && octal.bytes().all(|byte| matches!(byte, b'0'..=b'7'));
    }
    false
}

struct Cursor<'input> {
    events: std::vec::IntoIter<(Event<'input>, Span)>,
    buffered: Option<(Event<'input>, Span)>,
    nodes: usize,
}

impl<'input> Cursor<'input> {
    fn new(events: Vec<(Event<'input>, Span)>) -> Self {
        Self {
            events: events.into_iter(),
            buffered: None,
            nodes: 0,
        }
    }

    fn next(&mut self) -> Option<(Event<'input>, Span)> {
        self.buffered.take().or_else(|| self.events.next())
    }

    fn put_back(&mut self, event: (Event<'input>, Span)) {
        debug_assert!(self.buffered.is_none());
        self.buffered = Some(event);
    }

    fn expect_stream_start(&mut self) -> Result<(), Error> {
        match self.next() {
            Some((Event::StreamStart, _)) => Ok(()),
            _ => Err(Error::invalid("invalid YAML stream", None)),
        }
    }

    fn expect_document_start(&mut self) -> Result<(), Error> {
        match self.next() {
            Some((Event::DocumentStart(_), _)) => Ok(()),
            Some((Event::StreamEnd, _)) | None => Err(Error::invalid("manifest is empty", None)),
            Some((_, span)) => Err(Error::invalid(
                "expected one YAML document",
                Some(span.into()),
            )),
        }
    }

    fn expect_document_end(&mut self) -> Result<(), Error> {
        match self.next() {
            Some((Event::DocumentEnd, _)) => Ok(()),
            Some((_, span)) => Err(Error::invalid(
                "unexpected content after manifest document",
                Some(span.into()),
            )),
            None => Err(Error::invalid("incomplete YAML document", None)),
        }
    }

    fn expect_stream_end(&mut self) -> Result<(), Error> {
        match self.next() {
            Some((Event::StreamEnd, _)) if self.next().is_none() => Ok(()),
            Some((Event::DocumentStart(_), span)) => Err(Error::invalid(
                "manifest must contain exactly one YAML document",
                Some(span.into()),
            )),
            Some((_, span)) => Err(Error::invalid(
                "unexpected content after manifest document",
                Some(span.into()),
            )),
            None => Err(Error::invalid("incomplete YAML stream", None)),
        }
    }

    fn node(&mut self, depth: usize) -> Result<Node, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::invalid(
                format!("manifest exceeds the maximum nesting depth of {MAX_DEPTH}"),
                None,
            ));
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(Error::invalid(
                format!("manifest exceeds the maximum node count of {MAX_NODES}"),
                None,
            ));
        }

        let (event, span) = self
            .next()
            .ok_or_else(|| Error::invalid("unexpected end of YAML document", None))?;
        let location = Location::from(span);
        match event {
            Event::Alias(_) => Err(Error::invalid(
                "YAML aliases are not allowed",
                Some(location),
            )),
            Event::Scalar(value, style, anchor, tag) => {
                reject_properties(anchor, tag.is_some(), location)?;
                if value.len() > MAX_SCALAR_BYTES {
                    return Err(Error::invalid(
                        format!("scalar exceeds the {MAX_SCALAR_BYTES}-byte limit"),
                        Some(location),
                    ));
                }
                Ok(Node::Scalar(Scalar {
                    value: value.into_owned(),
                    style,
                    location,
                }))
            }
            Event::SequenceStart(anchor, tag) => {
                reject_properties(anchor, tag.is_some(), location)?;
                let mut values = Vec::new();
                loop {
                    let event = self
                        .next()
                        .ok_or_else(|| Error::invalid("unterminated YAML sequence", None))?;
                    if matches!(event.0, Event::SequenceEnd) {
                        break;
                    }
                    self.put_back(event);
                    if values.len() >= MAX_COLLECTION_ITEMS {
                        return Err(Error::invalid(
                            format!(
                                "sequence exceeds the maximum item count of {MAX_COLLECTION_ITEMS}"
                            ),
                            Some(location),
                        ));
                    }
                    values.push(self.node(depth + 1)?);
                }
                Ok(Node::Sequence(values, location))
            }
            Event::MappingStart(anchor, tag) => {
                reject_properties(anchor, tag.is_some(), location)?;
                let mut values = BTreeMap::new();
                let mut keys = BTreeSet::new();
                loop {
                    let event = self
                        .next()
                        .ok_or_else(|| Error::invalid("unterminated YAML mapping", None))?;
                    if matches!(event.0, Event::MappingEnd) {
                        break;
                    }
                    let key_location = Location::from(event.1);
                    let key = match event.0 {
                        Event::Scalar(value, _, anchor, tag) => {
                            reject_properties(anchor, tag.is_some(), key_location)?;
                            value.into_owned()
                        }
                        Event::Alias(_) => {
                            return Err(Error::invalid(
                                "YAML aliases are not allowed",
                                Some(key_location),
                            ));
                        }
                        _ => {
                            return Err(Error::invalid(
                                "mapping keys must be scalar strings",
                                Some(key_location),
                            ));
                        }
                    };
                    if key == "<<" {
                        return Err(Error::invalid(
                            "YAML merge keys are not allowed",
                            Some(key_location),
                        ));
                    }
                    if !keys.insert(key.clone()) {
                        return Err(Error::invalid(
                            format!("duplicate mapping key {key:?}"),
                            Some(key_location),
                        ));
                    }
                    if values.len() >= MAX_COLLECTION_ITEMS {
                        return Err(Error::invalid(
                            format!(
                                "mapping exceeds the maximum entry count of {MAX_COLLECTION_ITEMS}"
                            ),
                            Some(location),
                        ));
                    }
                    let value = self.node(depth + 1)?;
                    values.insert(
                        key,
                        MappingEntry {
                            key_location,
                            value,
                        },
                    );
                }
                Ok(Node::Mapping(values, location))
            }
            _ => Err(Error::invalid(
                "unexpected YAML event while reading a value",
                Some(location),
            )),
        }
    }
}

fn reject_properties(anchor: usize, tagged: bool, location: Location) -> Result<(), Error> {
    if anchor != 0 {
        return Err(Error::invalid(
            "YAML anchors are not allowed",
            Some(location),
        ));
    }
    if tagged {
        return Err(Error::invalid(
            "explicit YAML tags are not allowed",
            Some(location),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ErrorKind, Manifest, Service, parse};

    const VALID: &str = r#"
apiVersion: termux-stacks/v1alpha1
kind: Stack
metadata:
  name: hello
services:
  app:
    image: docker.io/library/alpine:3.22
    command: ["/bin/sh", "-c", "printf hello"]
"#;

    #[test]
    fn parses_the_vertical_slice() {
        assert_eq!(
            parse(VALID).expect("valid manifest"),
            Manifest {
                name: "hello".into(),
                service: Service {
                    name: "app".into(),
                    image: "docker.io/library/alpine:3.22".into(),
                    command: Some(vec!["/bin/sh".into(), "-c".into(), "printf hello".into()]),
                },
            }
        );
    }

    #[test]
    fn rejects_duplicate_keys() {
        let error = parse(&VALID.replace("  name: hello", "  name: hello\n  name: again"))
            .expect_err("duplicate must fail");
        assert_eq!(error.kind(), ErrorKind::Invalid);
        assert!(error.to_string().contains("duplicate mapping key \"name\""));
    }

    #[test]
    fn rejects_aliases_and_anchors() {
        let yaml = VALID.replace(
            "    image: docker.io/library/alpine:3.22",
            "    image: &image docker.io/library/alpine:3.22\n    command: [*image]",
        );
        let error = parse(&yaml).expect_err("anchors must fail");
        assert!(error.to_string().contains("anchors are not allowed"));
    }

    #[test]
    fn rejects_multiple_documents() {
        let error =
            parse(&format!("{VALID}\n---\n{VALID}")).expect_err("multiple documents must fail");
        assert!(error.to_string().contains("exactly one YAML document"));
    }

    #[test]
    fn rejects_unimplemented_mvp_fields() {
        let yaml = VALID.replace("    command:", "    environment: {A: B}\n    command:");
        let error = parse(&yaml).expect_err("environment is not in the vertical slice");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert!(error.to_string().contains("services.app.environment"));
    }

    #[test]
    fn rejects_an_empty_command() {
        let yaml = VALID.replace("[\"/bin/sh\", \"-c\", \"printf hello\"]", "[]");
        let error = parse(&yaml).expect_err("empty command must fail");
        assert!(error.to_string().contains("at least one argument"));
    }

    #[test]
    fn rejects_plain_non_string_command_values() {
        let yaml = VALID.replace("[\"/bin/sh\", \"-c\", \"printf hello\"]", "[true]");
        let error = parse(&yaml).expect_err("boolean is not a string");
        assert!(error.to_string().contains("command[0] must be a string"));
    }

    #[test]
    fn rejects_multiple_services() {
        let yaml = format!("{VALID}\n  second:\n    image: alpine:3.22\n");
        let error = parse(&yaml).expect_err("multiple services are not in the vertical slice");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn rejects_an_engine_option_as_an_image() {
        let yaml = VALID.replace("docker.io/library/alpine:3.22", "--override-alias");
        let error = parse(&yaml).expect_err("engine option injection must fail");
        assert!(error.to_string().contains("not start with '-'"));
    }
}
