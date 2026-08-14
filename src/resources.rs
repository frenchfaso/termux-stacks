use crate::engine::Bind;
use crate::manifest::{Manifest, MountKind, Service};
use crate::paths::RuntimePaths;
use std::fmt;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) enum Error {
    Io(io::Error),
    InvalidBase(PathBuf),
    InvalidBind(PathBuf),
    PortUnavailable(u16, io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "resource preparation failed: {error}"),
            Self::InvalidBase(path) => write!(
                formatter,
                "manifest base is not a canonical real directory: {}",
                path.display()
            ),
            Self::InvalidBind(path) => write!(
                formatter,
                "bind source is unavailable or cannot be represented safely: {}",
                path.display()
            ),
            Self::PortUnavailable(port, error) => {
                write!(formatter, "loopback port {port} is unavailable: {error}")
            }
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn resolve_binds(
    paths: &RuntimePaths,
    manifest_base: &str,
    manifest: &Manifest,
    service: &Service,
) -> Result<Vec<Bind>, Error> {
    let canonical_base = canonical_manifest_base(manifest_base)?;

    let mut bindings = Vec::with_capacity(service.mounts.len());
    for mount in &service.mounts {
        let source = match mount.kind {
            MountKind::Volume => {
                debug_assert!(manifest.volumes.contains(&mount.source));
                paths.prepare_volume_directory(&manifest.name, &mount.source)?
            }
            MountKind::Bind => resolve_bind_source(&canonical_base, &mount.source)?,
        };
        if source.to_str().is_none()
            || source.to_string_lossy().contains(':')
            || !fs::symlink_metadata(&source)
                .is_ok_and(|metadata| !metadata.file_type().is_symlink())
        {
            return Err(Error::InvalidBind(source));
        }
        bindings.push(Bind {
            source,
            target: mount.target.clone(),
        });
    }
    Ok(bindings)
}

pub(crate) fn validate_bind_sources(manifest_base: &str, manifest: &Manifest) -> Result<(), Error> {
    let canonical_base = canonical_manifest_base(manifest_base)?;
    for service in manifest.services.values() {
        for mount in &service.mounts {
            if mount.kind == MountKind::Bind {
                resolve_bind_source(&canonical_base, &mount.source)?;
            }
        }
    }
    Ok(())
}

fn canonical_manifest_base(manifest_base: &str) -> Result<PathBuf, Error> {
    let base = PathBuf::from(manifest_base);
    let canonical_base = fs::canonicalize(&base).map_err(|_| Error::InvalidBase(base.clone()))?;
    if !base.is_absolute()
        || canonical_base != base
        || !fs::metadata(&canonical_base).is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(Error::InvalidBase(base));
    }
    Ok(canonical_base)
}

fn resolve_bind_source(canonical_base: &Path, configured: &str) -> Result<PathBuf, Error> {
    let configured = Path::new(configured);
    let candidate = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        canonical_base.join(configured)
    };
    let source = fs::canonicalize(&candidate).map_err(|_| Error::InvalidBind(candidate))?;
    if source.to_str().is_none()
        || source.to_string_lossy().contains(':')
        || !fs::symlink_metadata(&source).is_ok_and(|metadata| !metadata.file_type().is_symlink())
    {
        return Err(Error::InvalidBind(source));
    }
    Ok(source)
}

pub(crate) fn preflight_ports(service: &Service) -> Result<(), Error> {
    let mut listeners = Vec::with_capacity(service.ports.len());
    for port in &service.ports {
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port.port);
        listeners.push(
            TcpListener::bind(address).map_err(|error| Error::PortUnavailable(port.port, error))?,
        );
    }
    drop(listeners);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Error, preflight_ports, resolve_binds, validate_bind_sources};
    use crate::manifest;
    use crate::paths::RuntimePaths;
    use std::fs;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

    #[test]
    fn resolves_relative_binds_and_persistent_volumes() {
        let prefix = crate::paths::test_prefix("resources");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare runtime");
        let project = prefix.join("project");
        fs::create_dir(&project).expect("project");
        fs::create_dir(project.join("config")).expect("config");
        let manifest = manifest::parse(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: demo\nvolumes:\n  data: {}\nservices:\n  app:\n    image: fake:test\n    mounts:\n      - {type: bind, source: ./config, target: /config}\n      - {type: volume, source: data, target: /data}\n",
        )
        .expect("manifest");

        let bindings = resolve_binds(
            &paths,
            project.to_str().expect("UTF-8"),
            &manifest,
            &manifest.services["app"],
        )
        .expect("bindings");
        assert_eq!(bindings[0].source, project.join("config"));
        assert_eq!(bindings[1].source, paths.volume_path("demo", "data"));
        assert!(bindings[1].source.is_dir());
        fs::remove_dir_all(prefix).expect("remove prefix");
    }

    #[test]
    fn detects_an_occupied_loopback_port() {
        let listener =
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
        let port = listener.local_addr().expect("address").port();
        let manifest = manifest::parse(&format!(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: demo\nservices:\n  app:\n    image: fake:test\n    ports: [{{address: 127.0.0.1, port: {port}}}]\n"
        ))
        .expect("manifest");
        assert!(matches!(
            preflight_ports(&manifest.services["app"]),
            Err(Error::PortUnavailable(actual, _)) if actual == port
        ));
    }

    #[test]
    fn offline_validation_rejects_a_missing_bind_source_without_creating_volumes() {
        let prefix = crate::paths::test_prefix("offline-binds");
        let project = prefix.join("project");
        fs::create_dir(&project).expect("project");
        let manifest = manifest::parse(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: demo\nvolumes:\n  data: {}\nservices:\n  app:\n    image: fake:test\n    mounts:\n      - {type: bind, source: ./missing, target: /config}\n      - {type: volume, source: data, target: /data}\n",
        )
        .expect("manifest");

        assert!(matches!(
            validate_bind_sources(project.to_str().expect("UTF-8"), &manifest),
            Err(Error::InvalidBind(path)) if path == project.join("missing")
        ));
        assert!(
            !prefix
                .join("var/lib/termux-stacks/volumes/demo/data")
                .exists()
        );
        fs::remove_dir_all(prefix).expect("remove prefix");
    }
}
