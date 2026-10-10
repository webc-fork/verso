//! Serving of Verso's bundled resources to Servo.
//!
//! Two things live here:
//!
//! - A [`ResourceReaderMethods`](servo::resources::ResourceReaderMethods)
//!   implementation, registered with Servo through the
//!   `submit_resource_reader!` inventory macro, that reads Servo's named
//!   resources from Verso's resource directory.
//! - A [`ProtocolHandler`](servo::protocol_handler::ProtocolHandler) that
//!   serves the same directory over the `verso://` scheme.

use std::path::Path;
use std::sync::OnceLock;

use log::warn;
use servo::protocol_handler::{
    DoneChannel, FetchContext, NetworkError, ProtocolHandler, Request, ResourceFetchTiming,
    Response, ResponseBody,
};
use servo::resources::{Resource, ResourceReaderMethods};

/// The global resource reader, registered with Servo through the
/// `submit_resource_reader!` inventory macro. The resource directory is set
/// once during startup through [`set_resource_dir`].
static RESOURCE_READER: ResourceReader = ResourceReader {
    resource_dir: OnceLock::new(),
};

servo::submit_resource_reader!(&RESOURCE_READER);

/// Set the directory that Verso's resources are read from.
///
/// Called once during startup, after the configuration has been parsed.
pub fn set_resource_dir(resource_dir: std::path::PathBuf) {
    RESOURCE_READER
        .resource_dir
        .set(resource_dir)
        .expect("The resource directory is already initialized");
}

/// The configured resource directory, if it has been set already.
fn resource_dir() -> Option<&'static std::path::PathBuf> {
    RESOURCE_READER.resource_dir.get()
}

/// Reads Servo's named resources from Verso's resource directory.
struct ResourceReader {
    resource_dir: OnceLock<std::path::PathBuf>,
}

impl ResourceReaderMethods for ResourceReader {
    fn read(&self, resource: Resource) -> Vec<u8> {
        let Some(resource_dir) = self.resource_dir.get() else {
            warn!("The resource directory was not initialized, serving an empty resource");
            return Vec::new();
        };
        let path = resource_dir.join(resource.filename());
        std::fs::read(&path).unwrap_or_else(|error| match resource {
            // The broken image icon is the only resource that must contain
            // valid image bytes; the others can be empty and Servo falls back
            // to its built-in defaults.
            Resource::BrokenImageIcon => {
                warn!(
                    "Failed to read the broken image icon at {}: {error}, using the built-in image",
                    path.display()
                );
                include_bytes!("../resources/rippy.png").to_vec()
            }
            _ => {
                warn!(
                    "Failed to read the resource at {}: {error}, serving an empty resource",
                    path.display()
                );
                Vec::new()
            }
        })
    }

    fn sandbox_access_files(&self) -> Vec<std::path::PathBuf> {
        vec![]
    }

    fn sandbox_access_files_dirs(&self) -> Vec<std::path::PathBuf> {
        vec![]
    }
}

/// Serves Verso's resource directory over the `verso://` scheme.
pub struct VersoProtocolHandler;

/// The content type served for a file extension, defaulting to
/// `application/octet-stream`.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("json") => "application/json",
        Some("html") => "text/html",
        _ => "application/octet-stream",
    }
}

impl ProtocolHandler for VersoProtocolHandler {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send + 'a>> {
        let response = match resource_dir() {
            Some(resource_dir) => {
                let current_url = request.current_url();
                let path = current_url.path();
                let path = resource_dir.join(path.strip_prefix('/').unwrap_or(path));

                match std::fs::read(&path) {
                    Ok(file) => {
                        let mut response = Response::new(
                            request.current_url(),
                            ResourceFetchTiming::new(request.timing_type()),
                        );
                        response.headers.insert(
                            http::header::CONTENT_TYPE,
                            http::header::HeaderValue::from_static(content_type(&path)),
                        );
                        *response.body.lock() = ResponseBody::Done(file);
                        response
                    }
                    Err(error) => {
                        warn!(
                            "Failed to serve {} over the `verso` scheme: {error}",
                            path.display()
                        );
                        Response::network_error(NetworkError::ResourceLoadError(format!(
                            "Opening the file {} failed: {error}",
                            path.display()
                        )))
                    }
                }
            }
            None => {
                warn!("The resource directory was not initialized");
                Response::network_error(NetworkError::ResourceLoadError(
                    "The resource directory was not initialized".to_string(),
                ))
            }
        };

        Box::pin(std::future::ready(response))
    }
}
