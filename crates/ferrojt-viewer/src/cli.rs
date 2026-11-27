//! The viewer's command line.
//!
//! Deliberately hand-rolled rather than `clap`: the binary takes one file and
//! one option, and the interesting part is the value of that option — the
//! camera text of [`CameraPose`], which is the same string the top bar shows
//! while a view is being moved. Whoever reads a bad pose off the screen and
//! types it back gets the same error message the parser would print.

use std::path::PathBuf;

use crate::camera::CameraPose;

/// What the command line asked for.
#[derive(Debug, Default)]
pub struct Options {
    /// The file to open, if any — without one the viewer starts empty.
    pub path: Option<PathBuf>,
    /// The view to open it in.
    pub camera: Option<CameraPose>,
    /// `--help` was asked for.
    pub help: bool,
}

pub const USAGE: &str = "\
ferrojt-viewer — look at an OpenJT, glTF or GLB file.

Usage:
  ferrojt-viewer [FILE] [OPTIONS]

Options:
  -c, --camera \"eye x,y,z at x,y,z\"
         open FILE with the eye and the target point given in model units
         (millimetres for a JT file). The top bar of the window shows the same
         text for the view you are looking at, and the C key prints it here, so
         a view can be quoted back on the command line.
  -h, --help
         print this text

Examples:
  ferrojt-viewer data\\ExampleFiles\\example_block_jt9.5.jt
  ferrojt-viewer part.jt --camera \"eye 12.34,-56.78,90.12 at 1.23,4.56,7.89\"
";

/// Parse `args` — the command line **without** the program name.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut out = Options::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        // `--camera=VALUE` and `--camera VALUE` mean the same thing.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg, None),
        };
        match flag {
            "-h" | "--help" => out.help = true,
            "-c" | "--camera" => {
                let value = match inline {
                    Some(value) => value.to_string(),
                    None => {
                        let Some(value) = args.get(index) else {
                            return Err("--camera needs a value".to_string());
                        };
                        index += 1;
                        value.clone()
                    }
                };
                out.camera = Some(
                    value
                        .parse()
                        .map_err(|error: String| format!("--camera: {error}"))?,
                );
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option {arg}"));
            }
            _ if out.path.is_some() => {
                return Err(format!("a second file was given: {arg}"));
            }
            _ => out.path = Some(PathBuf::from(arg)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiss3d::glamx::Vec3;

    fn args(text: &[&str]) -> Vec<String> {
        text.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn a_file_is_the_positional_argument() {
        let out = parse(&args(&["model.jt"])).expect("parsed");
        assert_eq!(out.path.as_deref(), Some(std::path::Path::new("model.jt")));
        assert!(out.camera.is_none());
        assert!(!out.help);
    }

    #[test]
    fn the_camera_takes_its_value_attached_or_apart() {
        let text = "eye 1,2,3 at 4,5,6";
        let apart = parse(&args(&["m.jt", "--camera", text])).expect("apart");
        let attached = parse(&args(&["m.jt", &format!("--camera={text}")])).expect("attached");
        let short = parse(&args(&["m.jt", "-c", text])).expect("short");
        assert_eq!(apart.camera, attached.camera);
        assert_eq!(apart.camera, short.camera);
        let pose = apart.camera.expect("a pose");
        assert_eq!(pose.eye, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(pose.at, Vec3::new(4.0, 5.0, 6.0));
    }

    #[test]
    fn the_order_of_the_arguments_does_not_matter() {
        let out = parse(&args(&["--camera", "at 4,5,6 eye 1,2,3", "model.jt"])).expect("parsed");
        assert_eq!(out.path.as_deref(), Some(std::path::Path::new("model.jt")));
        assert_eq!(out.camera.expect("pose").eye, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn help_is_not_an_error() {
        assert!(parse(&args(&["--help"])).expect("help").help);
        assert!(parse(&args(&[])).expect("empty").path.is_none());
    }

    #[test]
    fn a_bad_camera_is_reported_with_its_own_message() {
        let error = parse(&args(&["m.jt", "--camera", "eye 1,2 at 3,4,5"])).expect_err("bad");
        assert!(error.starts_with("--camera:"), "{error}");
        assert!(error.contains("coordinate triple"), "{error}");
    }

    #[test]
    fn an_unknown_option_is_an_error() {
        assert_eq!(
            parse(&args(&["--all-lods", "m.jt"])).expect_err("bad"),
            "unknown option --all-lods"
        );
        assert_eq!(
            parse(&args(&["m.jt", "other.jt"])).expect_err("bad"),
            "a second file was given: other.jt"
        );
        assert_eq!(
            parse(&args(&["m.jt", "--camera"])).expect_err("bad"),
            "--camera needs a value"
        );
    }
}
