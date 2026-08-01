use std::io::Read;

use herdr_file_viewer::open_target::{self, CliAction};

fn main() -> std::io::Result<()> {
    // Argv parsing lives in the library (`open_target::parse_args`) so it is unit-tested and so
    // unknown / bare flags degrade instead of killing a herdr-spawned pane.
    match open_target::parse_args(std::env::args().skip(1)) {
        CliAction::LaunchDecision => {
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json)?;
            println!("{}", herdr_file_viewer::launch::launch_decision(&json));
            Ok(())
        }
        CliAction::LaunchDecisionTab => {
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json)?;
            println!("{}", herdr_file_viewer::launch::launch_decision_tab(&json));
            Ok(())
        }
        CliAction::OpenedPaneId => {
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json)?;
            if let Some(pane_id) = herdr_file_viewer::launch::opened_pane_id(&json) {
                println!("{pane_id}");
            }
            Ok(())
        }
        CliAction::ViewerPaneLayout => {
            let (config, _) = herdr_file_viewer::config::load_config_from_env();
            let effective =
                herdr_file_viewer::config::resolve(&config, |key| std::env::var(key).ok());
            println!("{}", effective.viewer_pane_ratio.launcher_spec());
            Ok(())
        }
        CliAction::Run { open } => herdr_file_viewer::run(open),
    }
}
