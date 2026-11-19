use std::{env, io, io::Read, path::Path};

use cap_std::{ambient_authority, fs::Dir};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Request {
    /// UTF-8 text file paths relative to the configured project root, in return order.
    files: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExactFile {
    path: String,
    content: String,
    byte_length: usize,
    sha256: String,
}

#[derive(Serialize)]
struct Response {
    complete: bool,
    files: Vec<ExactFile>,
}

fn read_file(root: &Dir, path: &str) -> io::Result<ExactFile> {
    // Resolve and open relative to the held directory, even when symlinks change.
    if !root.metadata(path)?.is_file() {
        return Err(io::Error::other("expected a regular file"));
    }
    let mut file = root.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("expected a regular file"));
    }
    let mut content = String::new();
    file.read_to_string(&mut content).map_err(|err| {
        if err.kind() == io::ErrorKind::InvalidData {
            io::Error::new(err.kind(), "file contains invalid UTF-8")
        } else {
            err
        }
    })?;
    Ok(ExactFile {
        path: path.to_owned(),
        byte_length: content.len(),
        sha256: Sha256::digest(content.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        content,
    })
}

struct Reader {
    root: Dir,
}

#[tool_router]
impl Reader {
    #[tool(
        description = "Read complete UTF-8 project files when exact text, line endings, byte lengths, or SHA-256 hashes matter. Returns JSON in request order. Any file error fails the entire call.",
        annotations(read_only_hint = true)
    )]
    fn read_exact_files(&self, Parameters(request): Parameters<Request>) -> Result<String, String> {
        let mut files = Vec::with_capacity(request.files.len());
        for path in request.files {
            files.push(read_file(&self.root, &path).map_err(|err| format!("{path:?}: {err}"))?);
        }
        serde_json::to_string(&Response {
            complete: true,
            files,
        })
        .map_err(|err| err.to_string())
    }
}

#[tool_handler]
impl ServerHandler for Reader {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        )
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::var_os("PROJECT_ROOT")
        .filter(|p| !p.is_empty())
        .ok_or("PROJECT_ROOT must name a project directory")?;
    let root = Dir::open_ambient_dir(&path, ambient_authority())
        .map_err(|err| format!("PROJECT_ROOT {}: {err}", Path::new(&path).display()))?;
    let reader = Reader { root };
    reader.serve(stdio()).await?.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use tempfile::tempdir;

    fn call(root: &Path, paths: &[&str]) -> Result<Value, String> {
        let reader = Reader {
            root: Dir::open_ambient_dir(root, ambient_authority()).unwrap(),
        };
        let files = paths.iter().map(|p| (*p).to_owned()).collect();
        let text = reader.read_exact_files(Parameters(Request { files }))?;
        Ok(serde_json::from_str(&text).unwrap())
    }

    #[test]
    fn preserves_bytes_order_and_duplicates() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let text = " \t雪\r\nabc\n\n";
        fs::write(root.join("b"), text).unwrap();
        fs::write(root.join("a"), "").unwrap();
        let b = json!({
            "path": "b", "content": text, "byteLength": 12,
            "sha256": "c37c61b1975a903cb34b2030659fc06426452297f88e086aee9be6a0dcae03e4"
        });
        let a = json!({
            "path": "a", "content": "", "byteLength": 0,
            "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        });
        assert_eq!(
            call(root, &["b", "a", "b"]).unwrap(),
            json!({"complete": true, "files": [b, a, b]})
        );
    }

    #[test]
    fn any_failed_read_fails_the_batch() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("ok"), "first file content").unwrap();
        fs::write(dir.path().join("invalid"), [0xff]).unwrap();
        for path in ["missing", "invalid", "."] {
            let error = call(dir.path(), &["ok", path]).unwrap_err();
            assert!(error.contains(path));
            assert!(!error.contains("first file content"));
            if path == "invalid" {
                assert!(error.contains("UTF-8"));
            }
        }
    }

    #[test]
    fn rejects_paths_outside_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("project");
        let outside = dir.path().join("private");
        fs::create_dir(&root).unwrap();
        fs::write(&outside, "outside").unwrap();
        for path in ["../private", outside.to_str().unwrap()] {
            assert!(call(&root, &[path]).is_err());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("..", root.join("parent")).unwrap();
            symlink("../private", root.join("escape")).unwrap();
            for path in ["parent/private", "escape"] {
                assert!(call(&root, &[path]).is_err());
            }
            fs::write(root.join("local"), "inside").unwrap();
            symlink("local", root.join("link")).unwrap();
            assert_eq!(
                call(&root, &["link"]).unwrap()["files"][0]["content"],
                "inside"
            );
        }
    }

    #[test]
    fn large_file_survives_response_serialization() {
        let dir = tempdir().unwrap();
        let text = format!("START\n{}END", "abcdefgh\r\n".repeat(210_000));
        fs::write(dir.path().join("large"), &text).unwrap();
        let response = call(dir.path(), &["large"]).unwrap();
        assert_eq!(response["files"][0]["byteLength"], text.len());
        assert_eq!(
            response["files"][0]["content"].as_str(),
            Some(text.as_str())
        );
    }
}
