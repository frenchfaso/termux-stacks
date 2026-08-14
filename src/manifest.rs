use saphyr_parser::{Event, Parser, ScalarStyle, Span};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) name: String,
    pub(crate) services: BTreeMap<String, Service>,
    pub(crate) volumes: BTreeSet<String>,
}

impl Manifest {
    pub(crate) fn start_order(&self) -> Vec<String> {
        topological_order(&self.services)
            .expect("validated manifests have an acyclic dependency graph")
    }

    pub(crate) fn stop_order(&self) -> Vec<String> {
        let mut order = self.start_order();
        order.reverse();
        order
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Service {
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) mounts: Vec<Mount>,
    pub(crate) ports: Vec<Port>,
    pub(crate) depends_on: Vec<String>,
    pub(crate) restart: RestartPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MountKind {
    Volume,
    Bind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Mount {
    pub(crate) kind: MountKind,
    pub(crate) source: String,
    pub(crate) target: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Port {
    pub(crate) address: String,
    pub(crate) port: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum RestartPolicy {
    #[default]
    #[serde(rename = "no")]
    No,
    #[serde(rename = "on-failure")]
    OnFailure,
    #[serde(rename = "always")]
    Always,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    Io,
    Invalid,
}

impl ErrorKind {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Invalid => "invalid_manifest",
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
    reject_unknown_fields(
        &top,
        "manifest",
        &["apiVersion", "kind", "metadata", "services", "volumes"],
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
    reject_unknown_fields(&metadata, "metadata", &["name"])?;
    let name_node = take_required(&mut metadata, "name", "metadata.name")?;
    let name_location = name_node.location();
    let name = name_node.into_string("metadata.name")?;
    validate_name(&name, "metadata.name", Some(name_location))?;

    let volumes = parse_volumes(top.remove("volumes"))?;

    let services_node = take_required(&mut top, "services", "services")?;
    let services_location = services_node.location();
    let service_entries = services_node.into_mapping("services")?;
    if service_entries.is_empty() {
        return Err(Error::invalid(
            "services must contain at least one service",
            Some(services_location),
        ));
    }

    let mut services = BTreeMap::new();
    let mut dependency_locations = BTreeMap::new();
    let mut claimed_ports = BTreeMap::new();

    for (service_name, service_entry) in service_entries {
        validate_name(
            &service_name,
            "service name",
            Some(service_entry.key_location),
        )?;
        let service_path = format!("services.{service_name}");
        let service = parse_service(
            &service_name,
            service_entry.value,
            &service_path,
            &volumes,
            &mut dependency_locations,
            &mut claimed_ports,
        )?;
        services.insert(service_name, service);
    }

    for ((service_name, dependency), location) in &dependency_locations {
        if !services.contains_key(dependency) {
            return Err(Error::invalid(
                format!(
                    "services.{service_name}.dependsOn references unknown service {dependency:?}"
                ),
                Some(*location),
            ));
        }
    }

    if let Err(cyclic) = topological_order(&services) {
        return Err(Error::invalid(
            format!(
                "service dependency graph contains a cycle involving: {}",
                cyclic.join(", ")
            ),
            Some(services_location),
        ));
    }

    Ok(Manifest {
        name,
        services,
        volumes,
    })
}

fn parse_volumes(entry: Option<MappingEntry>) -> Result<BTreeSet<String>, Error> {
    let Some(entry) = entry else {
        return Ok(BTreeSet::new());
    };
    let declarations = entry.value.into_mapping("volumes")?;
    let mut volumes = BTreeSet::new();
    for (name, declaration) in declarations {
        validate_name(&name, "volume name", Some(declaration.key_location))?;
        let path = format!("volumes.{name}");
        let options = declaration.value.into_mapping(&path)?;
        reject_unknown_fields(&options, &path, &[])?;
        volumes.insert(name);
    }
    Ok(volumes)
}

fn parse_service(
    service_name: &str,
    node: Node,
    service_path: &str,
    volumes: &BTreeSet<String>,
    dependency_locations: &mut BTreeMap<(String, String), Location>,
    claimed_ports: &mut BTreeMap<u16, String>,
) -> Result<Service, Error> {
    let mut service = node.into_mapping(service_path)?;
    reject_unknown_fields(
        &service,
        service_path,
        &[
            "image",
            "command",
            "environment",
            "mounts",
            "ports",
            "dependsOn",
            "restart",
        ],
    )?;

    let image_path = format!("{service_path}.image");
    let image_node = take_required(&mut service, "image", &image_path)?;
    let image_location = image_node.location();
    let image = image_node.into_string(&image_path)?;
    if !(1..=2048).contains(&image.chars().count())
        || image.starts_with('-')
        || image.chars().any(char::is_control)
    {
        return Err(Error::invalid(
            format!(
                "{image_path} must be 1..=2048 characters, contain no control characters, and not start with '-'"
            ),
            Some(image_location),
        ));
    }

    let command = parse_command(service.remove("command"), service_path)?;
    let environment = parse_environment(service.remove("environment"), service_path)?;
    let mounts = parse_mounts(service.remove("mounts"), service_path, volumes)?;
    let ports = parse_ports(service.remove("ports"), service_path, claimed_ports)?;
    let depends_on = parse_dependencies(
        service.remove("dependsOn"),
        service_name,
        service_path,
        dependency_locations,
    )?;
    let restart = parse_restart(service.remove("restart"), service_path)?;

    Ok(Service {
        name: service_name.to_owned(),
        image,
        command,
        environment,
        mounts,
        ports,
        depends_on,
        restart,
    })
}

fn parse_command(
    entry: Option<MappingEntry>,
    service_path: &str,
) -> Result<Option<Vec<String>>, Error> {
    let Some(entry) = entry else {
        return Ok(None);
    };
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
        let argument_path = format!("{command_path}[{index}]");
        let location = value.location();
        let argument = value.into_string(&argument_path)?;
        if argument.contains('\0') {
            return Err(Error::invalid(
                format!("{argument_path} must not contain NUL"),
                Some(location),
            ));
        }
        command.push(argument);
    }
    if command[0].is_empty() {
        return Err(Error::invalid(
            format!("{command_path}[0] must not be empty"),
            Some(entry.key_location),
        ));
    }
    Ok(Some(command))
}

fn parse_environment(
    entry: Option<MappingEntry>,
    service_path: &str,
) -> Result<BTreeMap<String, String>, Error> {
    let Some(entry) = entry else {
        return Ok(BTreeMap::new());
    };
    let environment_path = format!("{service_path}.environment");
    let entries = entry.value.into_mapping(&environment_path)?;
    let mut environment = BTreeMap::new();
    for (key, value_entry) in entries {
        let key_path = format!("{environment_path}.{key}");
        if !valid_environment_name(&key) {
            return Err(Error::invalid(
                format!("environment key {key:?} must match ^[A-Za-z_][A-Za-z0-9_]*$"),
                Some(value_entry.key_location),
            ));
        }
        if reserved_environment_name(&key) {
            return Err(Error::invalid(
                format!("environment key {key:?} is reserved by the engine"),
                Some(value_entry.key_location),
            ));
        }
        let value_location = value_entry.value.location();
        let value = value_entry.value.into_string(&key_path)?;
        if value.contains('\0') {
            return Err(Error::invalid(
                format!("{key_path} must not contain NUL"),
                Some(value_location),
            ));
        }
        environment.insert(key, value);
    }
    Ok(environment)
}

fn parse_mounts(
    entry: Option<MappingEntry>,
    service_path: &str,
    volumes: &BTreeSet<String>,
) -> Result<Vec<Mount>, Error> {
    let Some(entry) = entry else {
        return Ok(Vec::new());
    };
    let mounts_path = format!("{service_path}.mounts");
    let entries = entry.value.into_sequence(&mounts_path)?;
    let mut mounts: Vec<Mount> = Vec::with_capacity(entries.len());
    for (index, node) in entries.into_iter().enumerate() {
        let mount_path = format!("{mounts_path}[{index}]");
        let mut mapping = node.into_mapping(&mount_path)?;
        reject_unknown_fields(&mapping, &mount_path, &["type", "source", "target"])?;

        let type_path = format!("{mount_path}.type");
        let type_node = take_required(&mut mapping, "type", &type_path)?;
        let type_location = type_node.location();
        let kind = match type_node.into_string(&type_path)?.as_str() {
            "volume" => MountKind::Volume,
            "bind" => MountKind::Bind,
            _ => {
                return Err(Error::invalid(
                    format!("{type_path} must be \"volume\" or \"bind\""),
                    Some(type_location),
                ));
            }
        };

        let source_path = format!("{mount_path}.source");
        let source_node = take_required(&mut mapping, "source", &source_path)?;
        let source_location = source_node.location();
        let source = source_node.into_string(&source_path)?;
        if source.is_empty() || source.contains('\0') {
            return Err(Error::invalid(
                format!("{source_path} must be a non-empty path"),
                Some(source_location),
            ));
        }
        if kind == MountKind::Volume {
            validate_name(&source, &source_path, Some(source_location))?;
            if !volumes.contains(&source) {
                return Err(Error::invalid(
                    format!("{source_path} references undeclared volume {source:?}"),
                    Some(source_location),
                ));
            }
        }

        let target_path = format!("{mount_path}.target");
        let target_node = take_required(&mut mapping, "target", &target_path)?;
        let target_location = target_node.location();
        let target = target_node.into_string(&target_path)?;
        if target.contains(':') {
            return Err(Error::invalid(
                format!("{target_path} must not contain ':'"),
                Some(target_location),
            ));
        }
        if !is_normalized_absolute_path(&target) {
            return Err(Error::invalid(
                format!(
                    "{target_path} must be an absolute normalized path without '.' or '..' components"
                ),
                Some(target_location),
            ));
        }
        if let Some(existing) = mounts
            .iter()
            .find(|existing| mount_targets_overlap(&existing.target, &target))
        {
            return Err(Error::invalid(
                format!(
                    "{target_path} overlaps mount target {:?} in the same service",
                    existing.target
                ),
                Some(target_location),
            ));
        }

        mounts.push(Mount {
            kind,
            source,
            target,
        });
    }
    Ok(mounts)
}

fn parse_ports(
    entry: Option<MappingEntry>,
    service_path: &str,
    claimed_ports: &mut BTreeMap<u16, String>,
) -> Result<Vec<Port>, Error> {
    let Some(entry) = entry else {
        return Ok(Vec::new());
    };
    let ports_path = format!("{service_path}.ports");
    let entries = entry.value.into_sequence(&ports_path)?;
    let mut ports = Vec::with_capacity(entries.len());
    for (index, node) in entries.into_iter().enumerate() {
        let port_path = format!("{ports_path}[{index}]");
        let mut mapping = node.into_mapping(&port_path)?;
        reject_unknown_fields(&mapping, &port_path, &["address", "port"])?;

        let address_path = format!("{port_path}.address");
        let address_node = take_required(&mut mapping, "address", &address_path)?;
        let address_location = address_node.location();
        let address = address_node.into_string(&address_path)?;
        if address != "127.0.0.1" {
            return Err(Error::invalid(
                format!("{address_path} must be \"127.0.0.1\""),
                Some(address_location),
            ));
        }

        let number_path = format!("{port_path}.port");
        let number_node = take_required(&mut mapping, "port", &number_path)?;
        let number_location = number_node.location();
        let number = number_node.into_integer(&number_path)?;
        if !(1024..=65535).contains(&number) {
            return Err(Error::invalid(
                format!("{number_path} must be between 1024 and 65535"),
                Some(number_location),
            ));
        }
        let port = number as u16;
        if let Some(first_path) = claimed_ports.insert(port, number_path.clone()) {
            return Err(Error::invalid(
                format!(
                    "{number_path} duplicates loopback port {port} already declared at {first_path}"
                ),
                Some(number_location),
            ));
        }
        ports.push(Port { address, port });
    }
    Ok(ports)
}

fn parse_dependencies(
    entry: Option<MappingEntry>,
    service_name: &str,
    service_path: &str,
    locations: &mut BTreeMap<(String, String), Location>,
) -> Result<Vec<String>, Error> {
    let Some(entry) = entry else {
        return Ok(Vec::new());
    };
    let dependencies_path = format!("{service_path}.dependsOn");
    let entries = entry.value.into_sequence(&dependencies_path)?;
    let mut dependencies = Vec::with_capacity(entries.len());
    let mut seen = BTreeSet::new();
    for (index, node) in entries.into_iter().enumerate() {
        let location = node.location();
        let path = format!("{dependencies_path}[{index}]");
        let dependency = node.into_string(&path)?;
        validate_name(&dependency, &path, Some(location))?;
        if !seen.insert(dependency.clone()) {
            return Err(Error::invalid(
                format!("{dependencies_path} contains duplicate service {dependency:?}"),
                Some(location),
            ));
        }
        locations.insert((service_name.to_owned(), dependency.clone()), location);
        dependencies.push(dependency);
    }
    Ok(dependencies)
}

fn parse_restart(entry: Option<MappingEntry>, service_path: &str) -> Result<RestartPolicy, Error> {
    let Some(entry) = entry else {
        return Ok(RestartPolicy::No);
    };
    let path = format!("{service_path}.restart");
    let location = entry.value.location();
    match entry.value.into_string(&path)?.as_str() {
        "no" => Ok(RestartPolicy::No),
        "on-failure" => Ok(RestartPolicy::OnFailure),
        "always" => Ok(RestartPolicy::Always),
        _ => Err(Error::invalid(
            format!("{path} must be \"no\", \"on-failure\", or \"always\""),
            Some(location),
        )),
    }
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn reserved_environment_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "ANDROID_ART_ROOT",
        "ANDROID_DATA",
        "ANDROID_I18N_ROOT",
        "ANDROID_ROOT",
        "ANDROID_RUNTIME_ROOT",
        "ANDROID_TZDATA_ROOT",
        "BOOTCLASSPATH",
        "COLORTERM",
        "DEX2OATBOOTCLASSPATH",
        "EXTERNAL_STORAGE",
        "HOME",
        "MOZ_FAKE_NO_SANDBOX",
        "PREFIX",
        "PULSE_SERVER",
        "TERM",
        "TMPDIR",
        "USER",
    ];
    name.starts_with("PROOT_") || name.starts_with("LD_") || RESERVED.contains(&name)
}

fn is_normalized_absolute_path(path: &str) -> bool {
    if !path.starts_with('/') || path.contains('\0') {
        return false;
    }
    if path == "/" {
        return true;
    }
    !path.ends_with('/')
        && path
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn mount_targets_overlap(first: &str, second: &str) -> bool {
    first == "/"
        || second == "/"
        || first == second
        || second
            .strip_prefix(first)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || first
            .strip_prefix(second)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn topological_order(services: &BTreeMap<String, Service>) -> Result<Vec<String>, Vec<String>> {
    let mut incoming = services
        .iter()
        .map(|(name, service)| (name.clone(), service.depends_on.len()))
        .collect::<BTreeMap<_, _>>();
    let mut dependents = services
        .keys()
        .map(|name| (name.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (name, service) in services {
        for dependency in &service.depends_on {
            if let Some(entries) = dependents.get_mut(dependency) {
                entries.insert(name.clone());
            }
        }
    }

    let mut ready = incoming
        .iter()
        .filter_map(|(name, count)| (*count == 0).then_some(name.clone()))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(services.len());
    while let Some(name) = ready.pop_first() {
        order.push(name.clone());
        for dependent in &dependents[&name] {
            let count = incoming
                .get_mut(dependent)
                .expect("all dependency targets were validated");
            *count -= 1;
            if *count == 0 {
                ready.insert(dependent.clone());
            }
        }
    }

    if order.len() == services.len() {
        Ok(order)
    } else {
        Err(incoming
            .into_iter()
            .filter_map(|(name, count)| (count != 0).then_some(name))
            .collect())
    }
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

fn reject_unknown_fields(
    mapping: &BTreeMap<String, MappingEntry>,
    path: &str,
    allowed: &[&str],
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

    fn into_integer(self, path: &str) -> Result<i64, Error> {
        match self {
            Self::Scalar(value) if value.style == ScalarStyle::Plain => {
                value.value.parse::<i64>().map_err(|_| {
                    Error::invalid(format!("{path} must be an integer"), Some(value.location))
                })
            }
            Self::Scalar(value) => Err(Error::invalid(
                format!("{path} must be an integer"),
                Some(value.location),
            )),
            other => Err(Error::invalid(
                format!("{path} must be an integer"),
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
                            if value.len() > MAX_SCALAR_BYTES {
                                return Err(Error::invalid(
                                    format!(
                                        "mapping key exceeds the {MAX_SCALAR_BYTES}-byte scalar limit"
                                    ),
                                    Some(key_location),
                                ));
                            }
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
    use super::{ErrorKind, MountKind, RestartPolicy, parse};

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
    fn parses_service_defaults() {
        let manifest = parse(VALID).expect("valid manifest");
        assert_eq!(manifest.name, "hello");
        assert!(manifest.volumes.is_empty());
        let service = &manifest.services["app"];
        assert_eq!(service.name, "app");
        assert_eq!(service.image, "docker.io/library/alpine:3.22");
        assert_eq!(
            service
                .command
                .as_ref()
                .expect("command")
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["/bin/sh", "-c", "printf hello"]
        );
        assert!(service.environment.is_empty());
        assert!(service.mounts.is_empty());
        assert!(service.ports.is_empty());
        assert!(service.depends_on.is_empty());
        assert_eq!(service.restart, RestartPolicy::No);
        assert_eq!(manifest.start_order(), ["app"]);
        assert_eq!(manifest.stop_order(), ["app"]);
    }

    #[test]
    fn parses_the_complete_mvp_schema() {
        let yaml = r#"
apiVersion: termux-stacks/v1alpha1
kind: Stack
metadata:
  name: notes
services:
  web:
    image: notes-web:2.3.0
    dependsOn: [api]
    environment:
      API_URL: http://127.0.0.1:8080
    mounts:
      - type: bind
        source: ./config
        target: /app/config
    restart: always
  api:
    image: notes-api:1.4.0
    command: ["--listen", "127.0.0.1:8080"]
    environment:
      DATA_DIR: /data
      EMPTY: ""
    mounts:
      - type: volume
        source: data
        target: /data
    ports:
      - address: 127.0.0.1
        port: 8080
    restart: on-failure
volumes:
  data: {}
"#;
        let manifest = parse(yaml).expect("full MVP manifest");
        assert_eq!(
            manifest.services.keys().cloned().collect::<Vec<_>>(),
            ["api", "web"]
        );
        assert_eq!(
            manifest.volumes.iter().cloned().collect::<Vec<_>>(),
            ["data"]
        );
        assert_eq!(manifest.start_order(), ["api", "web"]);
        assert_eq!(manifest.stop_order(), ["web", "api"]);

        let api = &manifest.services["api"];
        assert_eq!(api.environment["DATA_DIR"], "/data");
        assert_eq!(api.environment["EMPTY"], "");
        assert_eq!(api.mounts[0].kind, MountKind::Volume);
        assert_eq!(api.ports[0].port, 8080);
        assert_eq!(api.restart, RestartPolicy::OnFailure);

        let web = &manifest.services["web"];
        assert_eq!(web.depends_on, ["api"]);
        assert_eq!(web.mounts[0].kind, MountKind::Bind);
        assert_eq!(web.restart, RestartPolicy::Always);
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
    fn rejects_unknown_fields() {
        let yaml = VALID.replace("    command:", "    privileged: true\n    command:");
        let error = parse(&yaml).expect_err("unknown service field");
        assert_eq!(error.kind(), ErrorKind::Invalid);
        assert!(error.to_string().contains("services.app.privileged"));
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
    fn rejects_nul_in_command_arguments() {
        let yaml = VALID.replace(
            "[\"/bin/sh\", \"-c\", \"printf hello\"]",
            r#"["/bin/sh", "\0"]"#,
        );
        let error = parse(&yaml).expect_err("NUL cannot be represented in exec argv");
        assert!(
            error
                .to_string()
                .contains("command[1] must not contain NUL")
        );
    }

    #[test]
    fn rejects_an_empty_services_map() {
        let yaml = r#"
apiVersion: termux-stacks/v1alpha1
kind: Stack
metadata:
  name: hello
services: {}
"#;
        let error = parse(yaml).expect_err("services must be non-empty");
        assert!(error.to_string().contains("at least one service"));
    }

    #[test]
    fn rejects_an_engine_option_as_an_image() {
        let yaml = VALID.replace("docker.io/library/alpine:3.22", "--override-alias");
        let error = parse(&yaml).expect_err("engine option injection must fail");
        assert!(error.to_string().contains("not start with '-'"));
    }

    #[test]
    fn accepts_literal_environment_values() {
        let yaml = VALID.replace(
            "    command:",
            "    environment: {Alpha_1: value, EMPTY: \"\"}\n    command:",
        );
        let manifest = parse(&yaml).expect("ordinary literal environment");
        assert_eq!(manifest.services["app"].environment["Alpha_1"], "value");
    }

    #[test]
    fn rejects_non_string_environment_values() {
        let yaml = VALID.replace("    command:", "    environment: {COUNT: 3}\n    command:");
        let error = parse(&yaml).expect_err("environment values are strings");
        assert!(
            error
                .to_string()
                .contains("environment.COUNT must be a string")
        );
    }

    #[test]
    fn rejects_nul_in_environment_values() {
        let yaml = VALID.replace(
            "    command:",
            r#"    environment: {VALUE: "\0"}
    command:"#,
        );
        let error = parse(&yaml).expect_err("NUL cannot be represented in exec argv");
        assert!(
            error
                .to_string()
                .contains("environment.VALUE must not contain NUL")
        );
    }

    #[test]
    fn rejects_invalid_and_reserved_environment_names() {
        for key in ["1BAD", "BAD-NAME", "HOME", "PROOT_NEW_OPTION", "LD_PRELOAD"] {
            let yaml = VALID.replace(
                "    command:",
                &format!("    environment: {{{key}: value}}\n    command:"),
            );
            let error = parse(&yaml).expect_err("invalid or reserved key");
            assert!(
                error.to_string().contains("environment key"),
                "unexpected error for {key}: {error}"
            );
        }
    }

    #[test]
    fn volume_mounts_require_declarations() {
        let yaml = VALID.replace(
            "    command:",
            "    mounts: [{type: volume, source: data, target: /data}]\n    command:",
        );
        let error = parse(&yaml).expect_err("undeclared volume");
        assert!(error.to_string().contains("undeclared volume \"data\""));
    }

    #[test]
    fn volume_declarations_are_empty_mappings() {
        let yaml = format!("{VALID}\nvolumes:\n  data:\n    driver: local\n");
        let error = parse(&yaml).expect_err("volume options are not supported");
        assert!(
            error
                .to_string()
                .contains("unknown field volumes.data.driver")
        );
    }

    #[test]
    fn rejects_non_normalized_mount_targets() {
        for target in [
            "data",
            "/data/",
            "/data//config",
            "/data/./config",
            "/data/../config",
        ] {
            let yaml = VALID.replace(
                "    command:",
                &format!(
                    "    mounts: [{{type: bind, source: ./config, target: {target}}}]\n    command:"
                ),
            );
            let error = parse(&yaml).expect_err("target must be normalized and absolute");
            assert!(
                error.to_string().contains("absolute normalized path"),
                "unexpected error for {target}: {error}"
            );
        }
    }

    #[test]
    fn rejects_a_mount_target_that_cannot_be_encoded_by_the_engine() {
        let yaml = VALID.replace(
            "    command:",
            "    mounts: [{type: bind, source: ./config, target: /data:alternate}]\n    command:",
        );
        let error = parse(&yaml).expect_err("colon makes the engine bind argument ambiguous");
        assert!(
            error
                .to_string()
                .contains("services.app.mounts[0].target must not contain ':'")
        );
    }

    #[test]
    fn rejects_overlapping_mount_targets_within_a_service() {
        let yaml = VALID.replace(
            "    command:",
            "    mounts:\n      - {type: bind, source: ./data, target: /data}\n      - {type: bind, source: ./config, target: /data/config}\n    command:",
        );
        let error = parse(&yaml).expect_err("overlap must fail");
        assert!(error.to_string().contains("overlaps mount target"));
    }

    #[test]
    fn permits_the_same_mount_target_in_distinct_services() {
        let yaml = format!(
            "{VALID}\n  worker:\n    image: alpine:3.22\n    mounts: [{{type: bind, source: ./worker, target: /data}}]\n"
        )
        .replace(
            "    command:",
            "    mounts: [{type: bind, source: ./app, target: /data}]\n    command:",
        );
        parse(&yaml).expect("services have separate root filesystems");
    }

    #[test]
    fn enforces_fixed_unique_loopback_ports() {
        for declaration in [
            "{address: 0.0.0.0, port: 8080}",
            "{address: 127.0.0.1, port: 1023}",
            "{address: 127.0.0.1, port: 65536}",
            "{address: 127.0.0.1, port: \"8080\"}",
        ] {
            let yaml = VALID.replace(
                "    command:",
                &format!("    ports: [{declaration}]\n    command:"),
            );
            parse(&yaml).expect_err("invalid port declaration");
        }

        let duplicate = format!(
            "{VALID}\n  worker:\n    image: alpine:3.22\n    ports: [{{address: 127.0.0.1, port: 8080}}]\n"
        )
        .replace(
            "    command:",
            "    ports: [{address: 127.0.0.1, port: 8080}]\n    command:",
        );
        let error = parse(&duplicate).expect_err("manifest ports are globally unique");
        assert!(error.to_string().contains("duplicates loopback port 8080"));
    }

    #[test]
    fn rejects_unknown_duplicate_and_cyclic_dependencies() {
        let unknown = VALID.replace("    command:", "    dependsOn: [missing]\n    command:");
        let error = parse(&unknown).expect_err("unknown dependency");
        assert!(error.to_string().contains("unknown service \"missing\""));

        let duplicate =
            format!("{VALID}\n  worker:\n    image: alpine:3.22\n    dependsOn: [app, app]\n");
        let error = parse(&duplicate).expect_err("duplicate dependency");
        assert!(error.to_string().contains("duplicate service \"app\""));

        let cycle = VALID.replace("    command:", "    dependsOn: [worker]\n    command:")
            + "\n  worker:\n    image: alpine:3.22\n    dependsOn: [app]\n";
        let error = parse(&cycle).expect_err("dependency cycle");
        assert!(
            error
                .to_string()
                .contains("dependency graph contains a cycle")
        );
    }

    #[test]
    fn topological_order_is_lexically_deterministic() {
        let yaml = format!(
            "{VALID}\n  zebra:\n    image: alpine:3.22\n  api:\n    image: alpine:3.22\n    dependsOn: [app]\n  worker:\n    image: alpine:3.22\n    dependsOn: [app]\n"
        );
        let manifest = parse(&yaml).expect("valid dependency graph");
        assert_eq!(manifest.start_order(), ["app", "api", "worker", "zebra"]);
        assert_eq!(manifest.stop_order(), ["zebra", "worker", "api", "app"]);
    }

    #[test]
    fn rejects_invalid_restart_policy() {
        let yaml = VALID.replace("    command:", "    restart: unless-stopped\n    command:");
        let error = parse(&yaml).expect_err("unsupported restart policy");
        assert!(error.to_string().contains("on-failure"));
    }
}
