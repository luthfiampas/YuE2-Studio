//! Renders every screen of the launcher into a test buffer so a layout or
//! style change that panics, overflows a border or loses a title fails here
//! instead of in the terminal.

use crate::app::{App, Input, ModalState, PodAction, WorkspaceUsage};
use crate::runpod::{GpuType, LowestPrice, NetworkVolume, RunPodItem, RunPodRuntime};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::sync::Once;

static HOME: Once = Once::new();

/// `App::new` reads `~/.config/yue2-studio/launcher.toml`; point HOME at a
/// scratch directory so the test never touches the developer's config.
fn isolate_home() {
    HOME.call_once(|| {
        let dir = std::env::temp_dir().join(format!("yue2-launcher-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".config/yue2-studio"));
        unsafe {
            std::env::set_var("HOME", &dir);
        }
    });
}

fn app() -> App {
    isolate_home();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(None, tx)
}

fn gpu() -> GpuType {
    GpuType {
        id: "gpu-1".to_string(),
        display_name: "RTX 2000 Ada".to_string(),
        memory_in_gb: 16,
        secure_price: Some(0.74),
        community_price: Some(0.44),
        lowest_price: Some(LowestPrice {
            uninterruptable_price: Some(0.44),
            minimum_bid_price: None,
        }),
    }
}

fn volume() -> NetworkVolume {
    NetworkVolume {
        id: "vol-1".to_string(),
        name: "luthfi-ai-volume".to_string(),
        size: 35,
        data_center_id: Some("US-KS-2".to_string()),
    }
}

fn render(app: &App) -> (String, Vec<usize>) {
    let mut terminal = Terminal::new(TestBackend::new(110, 34)).expect("terminal");
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    let mut text = String::new();
    let mut row_cells = vec![0usize; buffer.area.height as usize];
    for (index, cell) in buffer.content.iter().enumerate() {
        let row = index / width;
        row_cells[row] += 1;
        text.push_str(cell.symbol());
        if (index + 1) % width == 0 {
            text.push('\n');
        }
    }
    (text, row_cells)
}

/// Every row holds exactly the terminal width in cells, so a line that runs
/// past the right edge cannot silently disappear.
fn assert_rectangular(row_cells: &[usize]) {
    assert_eq!(row_cells.len(), 34, "frame should have 34 rows");
    for (row, cells) in row_cells.iter().enumerate() {
        assert_eq!(*cells, 110, "row {row} is not exactly 110 cells");
    }
}

#[test]
fn dashboard_renders() {
    let app = app();
    let (frame, rows) = render(&app);
    assert_rectangular(&rows);
    assert!(frame.contains("Cloud Pods"), "pods panel title missing");
    assert!(
        frame.contains("Selected Pod Inspector"),
        "inspector title missing"
    );
    assert!(
        frame.contains("System Log Console"),
        "console title missing"
    );
}

#[test]
fn every_modal_renders_with_its_title() {
    let cases: Vec<(&str, ModalState)> = vec![
        (
            "Network Volume",
            ModalState::ProvisioningVolumeSelect {
                selected_volume_index: 0,
            },
        ),
        (
            "GPU for",
            ModalState::ProvisioningGpuSelect {
                selected_volume: Some(volume()),
                gpus: vec![gpu()],
                selected_index: 0,
            },
        ),
        (
            "Pod Name",
            ModalState::ProvisioningPodName {
                selected_volume: Some(volume()),
                gpu: gpu(),
                input: Input::new("2026-10-29-004100".to_string()),
            },
        ),
        (
            "Terminate Pod",
            ModalState::ConfirmPod {
                pod_id: "pod-1".to_string(),
                pod_name: "2026-10-29-004100".to_string(),
                action: PodAction::Terminate,
                selected_choice: 0,
            },
        ),
        (
            "Restart Daemon",
            ModalState::ConfirmRestartDaemon {
                host: "127.0.0.1".to_string(),
                port: 22,
                selected_choice: 1,
            },
        ),
        ("Keyboard Shortcuts", ModalState::Help { scroll: 0 }),
        (
            "Configuration",
            ModalState::Settings {
                selected_index: crate::app::THEME_ROW,
                editing: false,
                input: Input::new(String::new()),
            },
        ),
    ];

    for (title, modal) in cases {
        let mut app = app();
        app.modal = modal;
        let (frame, rows) = render(&app);
        assert_rectangular(&rows);
        assert!(
            frame.contains(title),
            "title {title:?} missing from the frame"
        );
    }
}

#[test]
fn dialogs_share_one_border_style() {
    let mut app = app();
    app.modal = ModalState::Settings {
        selected_index: 0,
        editing: false,
        input: Input::new(String::new()),
    };
    let (frame, _rows) = render(&app);
    // Rounded corners everywhere: double-line corners would mean a dialog
    // drifted away from the shared style module.
    assert!(
        frame.contains('╭') && frame.contains('╮'),
        "rounded top border missing"
    );
    assert!(!frame.contains('╔'), "a dialog still uses double borders");
}

#[test]
fn settings_only_lists_the_editable_fields() {
    let mut app = app();
    app.modal = ModalState::Settings {
        selected_index: 0,
        editing: false,
        input: Input::new(String::new()),
    };
    let (frame, _rows) = render(&app);
    assert!(frame.contains("Local Forward Port"), "port row missing");
    assert!(frame.contains("Server Poll (seconds)"), "poll row missing");
    assert!(frame.contains("Custom SSH Key File"), "ssh key row missing");
    assert!(frame.contains("Theme"), "theme row missing");
    // The container image is fixed in code now, so it is not a setting.
    assert!(
        !frame.contains("Template ID"),
        "template row still rendered"
    );
    assert!(!frame.contains("Docker Image"), "image row still rendered");
}

#[test]
fn inspector_body_starts_in_one_column() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    isolate_home();
    let app = App::new(Some(("127.0.0.1".to_string(), 22)), tx);
    let (frame, rows) = render(&app);
    assert_rectangular(&rows);
    // Every body line sits one cell inside the border. A double space means a
    // line is indented by its own string instead of the panel padding.
    assert!(
        frame.contains("│ Pod Name:"),
        "inspector field is not one cell from the border:\n{frame}"
    );
    assert!(
        !frame.contains("│  Pod Name:"),
        "inspector field is double-indented:\n{frame}"
    );
}

#[test]
fn workspace_row_belongs_to_the_selected_pod() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    isolate_home();
    let mut app = App::new(Some(("127.0.0.1".to_string(), 22)), tx);

    app.workspace = Some(WorkspaceUsage {
        pod_id: "manual-pod".to_string(),
        used_mb: 5_120,
    });
    let (frame, _rows) = render(&app);
    assert!(
        frame.contains("Workspace:"),
        "workspace row missing:\n{frame}"
    );
    assert!(
        frame.contains("5.0G used"),
        "wrong workspace numbers:\n{frame}"
    );

    // With a network volume attached, its declared size is the denominator.
    app.pods[0].network_volume = Some(volume());
    let (frame, _rows) = render(&app);
    assert!(
        frame.contains("5.0G / 35G (14%)"),
        "quota percentage missing:\n{frame}"
    );

    // Numbers read from another pod must not be shown against this one.
    app.workspace = Some(WorkspaceUsage {
        pod_id: "someone-else".to_string(),
        used_mb: 5_120,
    });
    let (frame, _rows) = render(&app);
    assert!(
        !frame.contains("Workspace:"),
        "stale pod's disk is shown:\n{frame}"
    );
}

#[test]
fn empty_inspector_matches_field_indent() {
    let app = app();
    let (frame, rows) = render(&app);
    assert_rectangular(&rows);
    assert!(
        frame.contains("│ No pod selected."),
        "empty state is not one cell from the border:\n{frame}"
    );
    // No blank row between the panel title and the message.
    let lines: Vec<&str> = frame.lines().collect();
    let index = lines
        .iter()
        .position(|line| line.contains("No pod selected."))
        .expect("empty state line");
    assert!(
        lines[index - 1].contains("Selected Pod Inspector"),
        "empty state has a blank row above it:\n{frame}"
    );
}

#[test]
fn theme_cycles_through_every_preset() {
    isolate_home();
    let first = crate::theme::cycle(crate::theme::PRESETS[0], 1);
    let mut seen = vec![first];
    let mut current = first;
    for _ in 1..crate::theme::PRESETS.len() {
        current = crate::theme::cycle(current, 1);
        seen.push(current);
    }
    assert_eq!(
        current,
        crate::theme::PRESETS[0],
        "cycling one full lap should land back on the first preset"
    );
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        crate::theme::PRESETS.len(),
        "cycle repeated a preset: {seen:?}"
    );
}

fn running_pod() -> RunPodItem {
    RunPodItem {
        id: "pod-1".to_string(),
        name: "pod".to_string(),
        desired_status: "RUNNING".to_string(),
        cost_per_hr: Some(0.5),
        runtime: Some(RunPodRuntime {
            uptime_in_seconds: Some(600),
            ports: Some(Vec::new()),
        }),
        machine: None,
        network_volume: None,
    }
}

#[test]
fn quitting_asks_before_leaving_pods_billing() {
    let mut app = app();
    app.pods.push(running_pod());

    app.handle_key_event(KeyEvent::from(KeyCode::Char('q')));
    assert!(!app.should_quit, "quit must wait for an answer");
    assert!(
        matches!(
            app.modal,
            ModalState::ConfirmQuit {
                running_pods: 1,
                ..
            }
        ),
        "running pods were not offered"
    );

    // Esc backs out without quitting.
    app.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(!app.should_quit, "Esc must cancel the quit");
    assert!(matches!(app.modal, ModalState::None));

    // "n" quits and leaves the pods running.
    app.handle_key_event(KeyEvent::from(KeyCode::Char('q')));
    app.handle_key_event(KeyEvent::from(KeyCode::Char('n')));
    assert!(app.should_quit, "'n' should quit anyway");
}

#[test]
fn quitting_without_running_pods_is_immediate() {
    let mut app = app();
    app.handle_key_event(KeyEvent::from(KeyCode::Char('q')));
    assert!(app.should_quit);
}

#[test]
fn estimated_cost_is_rate_times_uptime() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    isolate_home();
    let mut app = App::new(Some(("127.0.0.1".to_string(), 22)), tx);

    app.pods[0].cost_per_hr = Some(0.44);
    app.pods[0]
        .runtime
        .as_mut()
        .expect("runtime")
        .uptime_in_seconds = Some(3_600);
    let (frame, _rows) = render(&app);
    assert!(
        frame.contains("Est. Cost:"),
        "estimate row missing:\n{frame}"
    );
    assert!(frame.contains("$0.44"), "wrong estimate:\n{frame}");

    // Without an uptime there is nothing to bill against yet.
    app.pods[0]
        .runtime
        .as_mut()
        .expect("runtime")
        .uptime_in_seconds = None;
    let (frame, _rows) = render(&app);
    assert!(
        !frame.contains("Est. Cost:"),
        "estimate shown without uptime:\n{frame}"
    );
}
