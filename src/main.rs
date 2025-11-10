use ashpd::desktop::screenshot::Screenshot;
use clap::{ArgAction, Parser, command};
use std::{
    collections::HashMap,
    env, fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};
use zbus::{Connection, proxy, zvariant::Value};

mod localize;

#[derive(Parser, Default, Debug, Clone, PartialEq, Eq)]
#[command(version, about, long_about = None)]
struct Args {
    /// Enable interactive mode in the portal
    #[clap(long,
        default_missing_value("true"),
        default_value("true"),
        num_args(0..=1),
        require_equals(true),
        action = ArgAction::Set)]
    interactive: bool,
    /// Enable modal mode in the portal
    #[clap(long,
        default_missing_value("true"),
        default_value("true"),
        num_args(0..=1),
        require_equals(true),
        action = ArgAction::Set,)]
    modal: bool,
    /// Send a notification with the path to the saved screenshot
    #[clap(long,
        default_missing_value("true"),
        default_value("true"),
        num_args(0..=1),
        require_equals(true),
        action = ArgAction::Set)]
    notify: bool,
    /// The directory to save the screenshot to, if not performing an interactive screenshot
    #[clap(short, long)]
    save_dir: Option<PathBuf>,
    /// Open the captured screenshot in an annotation tool (brush/highlight/shapes)
    #[clap(long,
        default_missing_value("true"),
        default_value("false"),
        num_args(0..=1),
        require_equals(true),
        action = ArgAction::Set)]
    annotate: bool,
    /// Command to launch for annotation (must accept swappy-style -f/-o arguments)
    #[clap(long, default_value = "swappy")]
    annotate_tool: String,
}

#[proxy(assume_defaults = true)]
trait Notifications {
    /// Call the org.freedesktop.Notifications.Notify D-Bus method
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, &Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

//TODO: better error handling
#[tokio::main(flavor = "current_thread")]
async fn main() {
    crate::localize::localize();

    let args = Args::parse();
    if args.annotate {
        if let Err(err) = terminate_existing_annotators(&args.annotate_tool) {
            eprintln!("{err}");
        }
    }
    let previous_clipboard = if args.annotate {
        snapshot_clipboard_png()
    } else {
        None
    };
    let picture_dir = (!args.interactive).then(|| {
        args.save_dir
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(|| dirs::picture_dir().expect("failed to locate picture directory"))
    });

    let response = Screenshot::request()
        .interactive(args.interactive)
        .modal(args.modal)
        .send()
        .await
        .expect("failed to send screenshot request")
        .response()
        .expect("failed to receive screenshot response");

    let uri = response.uri();
    let (source, path) = match uri.scheme() {
        "file" => {
            let response_path = uri
                .to_file_path()
                .unwrap_or_else(|_| panic!("unsupported response URI '{uri}'"));
            if let Some(picture_dir) = picture_dir {
                let date = chrono::Local::now();
                let filename = format!("Screenshot_{}.png", date.format("%Y-%m-%d_%H-%M-%S"));
                let path = picture_dir.join(filename);
                if fs::metadata(&picture_dir)
                    .expect("Failed to get medatata on filesystem for screenshot destination")
                    .dev()
                    != fs::metadata(&response_path)
                        .expect("Failed to get metadata on filesystem for temporary path")
                        .dev()
                {
                    // copy file instead
                    fs::copy(&response_path, &path).expect("failed to move screenshot");
                    fs::remove_file(&response_path).expect("failed to remove temporary screenshot");
                } else {
                    fs::rename(&response_path, &path).expect("failed to move screenshot");
                }

                let display_path = path.to_string_lossy().to_string();
                (ScreenshotSource::File(path), display_path)
            } else {
                let display_path = response_path.to_string_lossy().to_string();
                (ScreenshotSource::File(response_path), display_path)
            }
        }
        "clipboard" => (
            ScreenshotSource::Clipboard {
                previous: previous_clipboard,
            },
            String::new(),
        ),
        scheme => panic!("unsupported scheme '{}'", scheme),
    };

    if args.annotate {
        if let Some(annotation) = prepare_annotation_input(&source) {
            if let Err(err) = annotate_with_tool(&args.annotate_tool, &annotation.path) {
                eprintln!("{err}");
            } else if annotation.reclip_after_edit {
                if let Err(err) = copy_png_to_clipboard(&annotation.path) {
                    eprintln!("{err}");
                }
            }

            if annotation.cleanup_after_use {
                if let Err(err) = fs::remove_file(&annotation.path) {
                    eprintln!("Failed to clean up temporary screenshot: {err}");
                }
            }
        } else {
            eprintln!(
                "Annotation requires access to the screenshot. Install 'wl-clipboard' (Wayland) or 'xclip' (X11) so clipboard captures can be annotated, or choose the Save option in the portal dialog."
            );
        }
    }

    println!("{path}");

    if args.notify {
        let connection = Connection::session()
            .await
            .expect("failed to connect to session bus");

        let message = if path.is_empty() {
            fl!("screenshot-saved-to-clipboard")
        } else {
            fl!("screenshot-saved-to")
        };
        let proxy = NotificationsProxy::new(&connection)
            .await
            .expect("failed to create proxy");
        _ = proxy
            .notify(
                &fl!("cosmic-screenshot"),
                0,
                "com.system76.CosmicScreenshot",
                &message,
                &path,
                &[],
                HashMap::from([("transient", &Value::Bool(true))]),
                5000,
            )
            .await
            .expect("failed to send notification");
    }
}

fn annotate_with_tool(command: &str, path: &Path) -> Result<(), String> {
    let status = Command::new(command)
        .arg("-f")
        .arg(path)
        .arg("-o")
        .arg(path)
        .status()
        .map_err(|err| format!("Failed to launch '{command}': {err}"))?;

    if !status.success() {
        return Err(format!(
            "Annotation tool '{command}' exited with status {status}",
        ));
    }

    Ok(())
}

#[derive(Debug)]
enum ScreenshotSource {
    File(PathBuf),
    Clipboard { previous: Option<Vec<u8>> },
}

struct AnnotationInput {
    path: PathBuf,
    cleanup_after_use: bool,
    reclip_after_edit: bool,
}

fn prepare_annotation_input(source: &ScreenshotSource) -> Option<AnnotationInput> {
    match source {
        ScreenshotSource::File(path) => Some(AnnotationInput {
            path: path.clone(),
            cleanup_after_use: false,
            reclip_after_edit: false,
        }),
        ScreenshotSource::Clipboard { previous } => {
            match persist_clipboard_png(previous.as_deref()) {
                Ok(temp_path) => Some(AnnotationInput {
                    path: temp_path,
                    cleanup_after_use: true,
                    reclip_after_edit: true,
                }),
                Err(err) => {
                    eprintln!("{err}");
                    None
                }
            }
        }
    }
}

fn persist_clipboard_png(previous: Option<&[u8]>) -> Result<PathBuf, String> {
    let png = grab_clipboard_png(previous)
        .map_err(|err| format!("Failed to read clipboard image: {err}"))?;

    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S%3f");
    let temp_path = env::temp_dir().join(format!("cosmic-screenshot-clipboard-{timestamp}.png"));
    fs::write(&temp_path, png)
        .map_err(|err| format!("Failed to persist clipboard screenshot: {err}"))?;

    Ok(temp_path)
}

fn grab_clipboard_png(previous: Option<&[u8]>) -> Result<Vec<u8>, String> {
    const ATTEMPTS: usize = 25;
    const WAIT_MS: u64 = 80;
    let mut last_err = String::new();
    let mut identical_reads = 0;

    for attempt in 0..ATTEMPTS {
        match read_clipboard_png_once() {
            Ok(bytes) => {
                if previous.map(|p| p == bytes.as_slice()).unwrap_or(false)
                    && attempt + 1 < ATTEMPTS
                    && identical_reads < 5
                {
                    identical_reads += 1;
                    thread::sleep(Duration::from_millis(WAIT_MS));
                    continue;
                }

                return Ok(bytes);
            }
            Err(err) => {
                last_err = err;
            }
        }

        if attempt + 1 < ATTEMPTS {
            thread::sleep(Duration::from_millis(WAIT_MS));
        }
    }

    Err(last_err)
}

fn read_clipboard_png_once() -> Result<Vec<u8>, String> {
    let mut errors = Vec::new();

    match capture_command_stdout("wl-paste", &["--no-newline", "--type", "image/png"]) {
        Ok(bytes) => return Ok(bytes),
        Err(err) => errors.push(format!("wl-paste: {err}")),
    }

    match capture_command_stdout(
        "xclip",
        &["-selection", "clipboard", "-t", "image/png", "-o"],
    ) {
        Ok(bytes) => return Ok(bytes),
        Err(err) => errors.push(format!("xclip: {err}")),
    }

    Err(format!(
        "Failed to read clipboard image via helpers:\n{}",
        errors.join("\n")
    ))
}

fn capture_command_stdout(command: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|err| format!("Failed to run '{command}': {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(format!("'{command}' exited with status {}", output.status));
        }
        return Err(format!(
            "'{command}' exited with status {} ({stderr})",
            output.status
        ));
    }

    if output.stdout.is_empty() {
        return Err(format!("'{command}' produced no clipboard data on stdout"));
    }

    Ok(output.stdout)
}

fn copy_png_to_clipboard(path: &Path) -> Result<(), String> {
    let data =
        fs::read(path).map_err(|err| format!("Failed to read annotated screenshot: {err}"))?;

    if pipe_into_command("wl-copy", &["--type", "image/png"], &data).is_ok() {
        return Ok(());
    }

    if pipe_into_command(
        "xclip",
        &["-selection", "clipboard", "-t", "image/png", "-i"],
        &data,
    )
    .is_ok()
    {
        return Ok(());
    }

    Err(
        "Unable to copy annotated screenshot to the clipboard (install wl-clipboard or xclip)."
            .to_string(),
    )
}

fn pipe_into_command(command: &str, args: &[&str], data: &[u8]) -> Result<(), String> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| format!("Failed to run '{command}': {err}"))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(data)
            .map_err(|err| format!("Failed to write image data to '{command}': {err}"))?;
    }

    let status = child
        .wait()
        .map_err(|err| format!("Failed to wait for '{command}': {err}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("'{command}' exited with status {status}"))
    }
}

fn snapshot_clipboard_png() -> Option<Vec<u8>> {
    read_clipboard_png_once().ok()
}

fn terminate_existing_annotators(command: &str) -> Result<(), String> {
    let process_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);

    if let Err(err) = attempt_process_kill("pkill", &["-x"], process_name) {
        if let Err(second_err) = attempt_process_kill("killall", &["-q"], process_name) {
            return Err(format!(
                "Failed to terminate existing '{process_name}' instances: {err}; {second_err}"
            ));
        }
    }

    Ok(())
}

fn attempt_process_kill(executable: &str, args: &[&str], process_name: &str) -> Result<(), String> {
    let status = Command::new(executable)
        .args(args)
        .arg(process_name)
        .status()
        .map_err(|err| format!("Failed to run '{executable}': {err}"))?;

    if status.success() || status.code() == Some(1) {
        return Ok(());
    }

    Err(format!(
        "'{executable}' exited with status {status} while terminating '{process_name}'",
    ))
}
