//! Renders every screen of the launcher into a terminal frame. All layout and
//! styling decisions come from `style` and `theme`; nothing here decides what
//! is on screen beyond the data it is handed.

use crate::app::{App, BuildTarget, Input, ModalState, PodAction, THEME_ROW};
use crate::runpod::PodState;
use crate::style::{self, GUTTER};
use crate::theme::current as theme;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap},
};

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(12),        // Main Split (Left: Pods / Volumes, Right: Inspector)
            Constraint::Length(GUTTER), // Gap
            Constraint::Length(10),     // Bottom Console & Rolling Logs
            Constraint::Length(1),      // Unified Status & Keybar
        ])
        .split(f.area());

    draw_main_split(f, app, chunks[0]);
    draw_console(f, app, chunks[2]);
    draw_status_bar(f, app, chunks[3]);

    // Modals
    match &app.modal {
        ModalState::ProvisioningVolumeSelect {
            selected_volume_index,
        } => {
            draw_volume_select_modal(f, app, *selected_volume_index);
        }
        ModalState::ProvisioningGpuSelect {
            selected_volume,
            gpus,
            selected_index,
        } => {
            draw_gpu_select_modal(f, selected_volume.as_ref(), gpus, *selected_index);
        }
        ModalState::ProvisioningPodName {
            selected_volume,
            gpu,
            input,
        } => {
            draw_pod_name_modal(f, selected_volume.as_ref(), gpu, input);
        }
        ModalState::ConfirmPod {
            pod_name,
            action,
            selected_choice,
            ..
        } => {
            draw_pod_confirm_modal(f, pod_name, *action, *selected_choice);
        }
        ModalState::ConfirmRestartDaemon {
            host,
            port,
            selected_choice,
        } => {
            draw_confirm_modal(
                f,
                "Restart Daemon",
                theme().accent,
                &[
                    &format!("Restart remote studio daemon on {}:{}?", host, port),
                    "Any audio generation currently in progress will be interrupted.",
                ],
                "Restart Daemon",
                "Cancel",
                *selected_choice,
            );
        }
        ModalState::ConfirmBuild {
            target,
            selected_choice,
        } => {
            draw_build_confirm_modal(f, *target, *selected_choice);
        }
        ModalState::ConfirmQuit {
            running_pods,
            selected_choice,
        } => {
            let lines = vec![
                format!("{} pod(s) are still running and billing.", running_pods),
                "Stopping them releases the GPU and pauses the bill;".to_string(),
                "their network volumes are preserved.".to_string(),
            ];
            draw_confirm_modal(
                f,
                "Quit",
                theme().danger,
                &refs(&lines),
                "Stop & Quit",
                "Quit Anyway",
                *selected_choice,
            );
        }
        ModalState::Settings {
            selected_index,
            editing,
            input,
        } => {
            draw_settings_modal(f, app, *selected_index, *editing, input);
        }
        ModalState::Help { scroll } => {
            draw_help_modal(f, *scroll);
        }
        ModalState::None => {}
    }
}

fn draw_main_split(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(38),
            Constraint::Length(GUTTER),
            Constraint::Min(0),
        ])
        .split(area);

    // Left: Pods & Network Volumes
    draw_pods_panel(f, app, chunks[0]);

    // Right: Inspector & Actions
    draw_inspector_panel(f, app, chunks[2]);
}

fn draw_pods_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut items = Vec::new();

    if app.pods.is_empty() {
        items.push(ListItem::new(Span::styled(
            "(no pods, press n to provision or r to sync)",
            Style::default().fg(theme().muted),
        )));
    } else {
        for (idx, pod) in app.pods.iter().enumerate() {
            let is_sel = idx == app.selected_pod_index;

            let (status_text, status_color) = status_label(pod.state());
            let status_style = Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD);

            let cost_str = pod
                .cost_per_hr
                .map(|c| format!("${:.2}/h", c))
                .unwrap_or_else(|| "On-Demand".to_string());
            let gpu_name = pod
                .machine
                .as_ref()
                .and_then(|m| m.gpu_display_name.clone())
                .unwrap_or_else(|| "GPU".to_string());

            let mut status_spans = vec![Span::raw(" ["), Span::styled(status_text, status_style)];
            if pod.desired_status.eq_ignore_ascii_case("RUNNING")
                && let Some(uptime_secs) = pod.runtime.as_ref().and_then(|r| r.uptime_in_seconds)
            {
                let (uptime_str, uptime_style) = format_uptime(Some(uptime_secs));
                status_spans.push(Span::raw(" • "));
                status_spans.push(Span::styled(uptime_str, uptime_style));
            }
            status_spans.push(Span::raw("] "));

            let mut line_spans = vec![
                Span::styled(style::marker(is_sel), Style::default().fg(theme().accent)),
                Span::styled(
                    &pod.name,
                    if is_sel {
                        Style::default()
                            .fg(theme().accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme().text)
                    },
                ),
            ];
            line_spans.extend(status_spans);
            line_spans.push(Span::styled(gpu_name, Style::default().fg(theme().muted)));
            line_spans.push(Span::styled(
                format!(" ({})", cost_str),
                Style::default().fg(theme().muted),
            ));

            items.push(ListItem::new(Line::from(line_spans)));
        }
    }

    let volume_count = app.network_volumes.len();
    let title = match volume_count {
        0 => "Cloud Pods".to_string(),
        1 => "Cloud Pods · 1 volume".to_string(),
        n => format!("Cloud Pods · {} volumes", n),
    };
    let block = style::panel(title, theme().info);

    let list = List::new(items).block(block);

    f.render_widget(list, area);
}

/// The single mapping from a pod state to its label and colour.
fn status_label(state: PodState) -> (&'static str, Color) {
    match state {
        PodState::Running => ("RUNNING", theme().ok),
        PodState::Initializing => ("INITIALIZING", theme().accent),
        PodState::Stopped => ("STOPPED", theme().muted),
        PodState::Offline => ("OFFLINE", theme().muted),
    }
}

fn format_uptime(uptime_secs: Option<u64>) -> (String, Style) {
    match uptime_secs {
        Some(secs) => {
            let formatted = if secs >= 3600 {
                format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
            } else {
                format!("{}m {}s", secs / 60, secs % 60)
            };
            let style = if secs >= 1800 {
                Style::default()
                    .fg(theme().danger)
                    .add_modifier(Modifier::BOLD)
            } else if secs >= 900 {
                Style::default().fg(theme().accent)
            } else {
                Style::default().fg(theme().ok)
            };
            (formatted, style)
        }
        None => ("Offline".to_string(), Style::default().fg(theme().muted)),
    }
}

/// The `used / total (pct)` cell shared by VRAM, RAM and the workspace
/// volume, so the three read alike instead of drifting apart.
fn usage_cell(used_gb: f64, total_gb: f64, percent: u64) -> String {
    format!("{used_gb:.1}G / {total_gb:.0}G ({percent}%)")
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        truncated.push('…');
        truncated
    }
}

fn draw_inspector_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = style::panel("Selected Pod Inspector", theme().info);

    let mut lines = Vec::new();

    if let Some(pod) = app.selected_pod() {
        let (ssh_ip, ssh_port) = pod
            .ssh_connection_info()
            .map(|(ip, p)| (ip, p.to_string()))
            .unwrap_or_else(|| {
                if pod.desired_status.eq_ignore_ascii_case("RUNNING") {
                    ("Initializing...".to_string(), "⏳".to_string())
                } else {
                    ("Offline".to_string(), "-".to_string())
                }
            });

        let gpu_display = pod
            .machine
            .as_ref()
            .and_then(|m| m.gpu_display_name.clone())
            .unwrap_or_else(|| "Unknown GPU".to_string());

        let (uptime_str, uptime_style) =
            format_uptime(pod.runtime.as_ref().and_then(|r| r.uptime_in_seconds));

        let (status_text, status_color) = status_label(pod.state());

        // 1. Pod Specifications & Network Details
        let wide = area.width >= 56;
        let col1_w = (area.width.saturating_sub(30) / 2).clamp(20, 36) as usize;

        let mut fields = vec![
            (
                "Pod Name",
                pod.name.clone(),
                Style::default()
                    .fg(theme().text)
                    .add_modifier(Modifier::BOLD),
            ),
            (
                "Status",
                status_text.to_string(),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            (
                "Instance",
                pod.id.clone(),
                Style::default().fg(theme().muted),
            ),
            ("Uptime", uptime_str, uptime_style),
            (
                "GPU Model",
                gpu_display,
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            ),
            (
                "Cost",
                format!("${:.2}/hr", pod.cost_per_hr.unwrap_or(0.0)),
                Style::default().fg(theme().ok),
            ),
        ];

        // Billed so far this session: rate times the uptime RunPod reports.
        if let (Some(rate), Some(secs)) = (
            pod.cost_per_hr,
            pod.runtime.as_ref().and_then(|r| r.uptime_in_seconds),
        ) {
            fields.push((
                "Est. Cost",
                format!("${:.2}", rate * secs as f64 / 3600.0),
                Style::default().fg(theme().ok),
            ));
        }

        fields.push((
            "SSH Host",
            format!("{}:{}", ssh_ip, ssh_port),
            Style::default().fg(theme().info),
        ));

        push_fields(&mut lines, fields, wide, col1_w);

        // Read over SSH, not from the studio server, so it belongs with the
        // pod's own details rather than under the telemetry heading.
        if let Some(usage) = app
            .workspace
            .as_ref()
            .filter(|usage| usage.pod_id == pod.id)
        {
            let used_gb = usage.used_mb as f64 / 1024.0;
            // A network volume's declared size is the quota; `df` reports the
            // backing store instead, so it is the only honest denominator.
            let quota_gb = pod.network_volume.as_ref().map(|volume| volume.size);
            let (text, percent) = match quota_gb {
                Some(total) if total > 0 => {
                    let percent = usage.used_mb * 100 / (total * 1024);
                    (usage_cell(used_gb, total as f64, percent), Some(percent))
                }
                _ => (format!("{used_gb:.1}G used"), None),
            };
            push_fields(
                &mut lines,
                vec![(
                    "Workspace",
                    text,
                    Style::default().fg(match percent {
                        Some(percent) if percent > 85 => theme().danger,
                        _ => theme().info,
                    }),
                )],
                wide,
                col1_w,
            );
        }

        lines.push(Line::from(""));

        // 2. Live Telemetry
        lines.push(style::section("Live Remote Telemetry"));

        let is_telemetry_live = app.tunnel_alive && app.is_server_healthy;

        if is_telemetry_live {
            if let Some(ref res) = app.last_resources {
                let ram_used = res.ram_used_mb as f64 / 1024.0;
                let ram_total = res.ram_total_mb as f64 / 1024.0;
                let ram_pct = if ram_total > 0.0 {
                    (ram_used / ram_total * 100.0) as u64
                } else {
                    0
                };

                let (vram_str, vram_pct) = if let Some(g) = res.gpus.first() {
                    let u = g.vram_used_mb as f64 / 1024.0;
                    let t = g.vram_total_mb as f64 / 1024.0;
                    let p = g.utilization_percent.unwrap_or(0);
                    (usage_cell(u, t, u64::from(p)), p)
                } else {
                    ("N/A".to_string(), 0)
                };

                push_fields(
                    &mut lines,
                    vec![
                        (
                            "VRAM",
                            vram_str,
                            Style::default().fg(if vram_pct > 85 {
                                theme().danger
                            } else {
                                theme().info
                            }),
                        ),
                        (
                            "CPU Load",
                            format!("{:.1}%", res.cpu_percent),
                            Style::default().fg(theme().text),
                        ),
                        (
                            "Sys RAM",
                            usage_cell(ram_used, ram_total, ram_pct),
                            Style::default().fg(theme().text),
                        ),
                    ],
                    wide,
                    col1_w,
                );
            } else {
                lines.push(Line::from(vec![
                    style::label("Telemetry"),
                    Span::styled(
                        "Awaiting live stream...",
                        Style::default().fg(theme().muted),
                    ),
                ]));
            }
        } else if app.tunnel_alive {
            // The tunnel is up, so this is the daemon, not the connection.
            lines.push(Line::from(vec![
                style::label("Telemetry"),
                Span::styled(
                    "Tunnel up · daemon not responding (press R)",
                    Style::default().fg(theme().accent),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                style::label("Telemetry"),
                Span::styled(
                    format!("Offline ({})", app.tunnel_status),
                    Style::default().fg(theme().muted),
                ),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No pod selected.",
            Style::default().fg(theme().muted),
        )));
    }

    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

/// Lays fields out two per line when there is room, one per line otherwise.
fn push_fields(
    lines: &mut Vec<Line<'static>>,
    fields: Vec<(&'static str, String, Style)>,
    wide: bool,
    col1_w: usize,
) {
    if !wide {
        for (label, value, style) in fields {
            lines.push(Line::from(vec![
                style::label(label),
                Span::styled(value, style),
            ]));
        }
        return;
    }

    for pair in fields.chunks(2) {
        let first = &pair[0];
        let mut spans = vec![
            style::label(first.0),
            Span::styled(
                format!("{:<w$}", truncate_str(&first.1, col1_w), w = col1_w),
                first.2,
            ),
        ];
        if let Some(second) = pair.get(1) {
            spans.push(style::column_gap());
            spans.push(style::label(second.0));
            spans.push(Span::styled(second.1.clone(), second.2));
        }
        lines.push(Line::from(spans));
    }
}

fn draw_console(f: &mut Frame, app: &App, area: Rect) {
    let is_search_active = app.log_search_query.is_some();
    let query_str = app.log_search_query.as_deref().unwrap_or("");

    let title = if is_search_active {
        if query_str.is_empty() {
            "System Log Console · search".to_string()
        } else {
            format!("System Log Console · \"{}\"", query_str)
        }
    } else {
        "System Log Console".to_string()
    };

    let border_color = if is_search_active {
        theme().accent
    } else {
        theme().muted
    };

    let block = style::panel(title, border_color);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Split inner area if search is active: logs on top, search input pinned at the bottom!
    let (log_area, search_area) = if is_search_active && inner.height > 1 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),    // Top: Log list
                Constraint::Length(1), // Bottom: Anchored search bar
            ])
            .split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    let matching_logs: Vec<&String> = if is_search_active && !query_str.is_empty() {
        let q_lower = query_str.to_lowercase();
        app.logs
            .iter()
            .filter(|l| l.to_lowercase().contains(&q_lower))
            .collect()
    } else {
        app.logs.iter().collect()
    };

    let mut wrapped_items: Vec<ListItem> = Vec::new();
    if is_search_active && !query_str.is_empty() && matching_logs.is_empty() {
        wrapped_items.push(ListItem::new(Span::styled(
            format!("  (No logs matching search query \"{}\")", query_str),
            Style::default().fg(theme().muted),
        )));
    } else {
        for log in &matching_logs {
            wrapped_items.extend(wrap_log_entry(log, log_area.width as usize));
        }
    }

    let total_lines = wrapped_items.len();
    let skip_count = total_lines.saturating_sub(log_area.height as usize);
    let visible_items: Vec<ListItem> = wrapped_items.into_iter().skip(skip_count).collect();

    let list = List::new(visible_items);
    f.render_widget(list, log_area);

    if let Some(s_area) = search_area {
        let match_info = if query_str.is_empty() {
            format!("({} logs)", matching_logs.len())
        } else {
            let count = matching_logs.len();
            format!("({} match{})", count, if count == 1 { "" } else { "es" })
        };

        let search_bar = Paragraph::new(Line::from(vec![
            Span::styled(
                " search ",
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(query_str, Style::default().fg(theme().text)),
            Span::styled("█", Style::default().fg(theme().accent)),
            Span::styled(
                format!("  {}  ", match_info),
                Style::default().fg(theme().text_dim),
            ),
            Span::styled(
                "[Esc]",
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" exit filter", Style::default().fg(theme().text_dim)),
        ]))
        .style(Style::default().bg(theme().surface));
        f.render_widget(search_bar, s_area);
    }
}

fn wrap_log_entry<'a>(log: &'a str, max_width: usize) -> Vec<ListItem<'a>> {
    let width = if max_width < 10 { 80 } else { max_width };

    let style = if log.contains("❌") || log.contains("Failed") || log.contains("Error") {
        Style::default().fg(theme().danger)
    } else if log.contains("✅") || log.contains("🎉") {
        Style::default().fg(theme().ok)
    } else if log.contains("Deploying")
        || log.contains("Building")
        || log.contains("Uploading")
        || log.contains("Starting")
    {
        Style::default().fg(theme().accent)
    } else {
        Style::default().fg(theme().text)
    };

    let continuation_indent = "  ↳ ";
    let continuation_width = width.saturating_sub(4).max(10);
    let mut items = Vec::new();

    for (line_idx, raw_line) in log.lines().enumerate() {
        let mut remaining = raw_line;
        let mut is_first_segment = line_idx == 0;

        while !remaining.is_empty() {
            let limit = if is_first_segment {
                width
            } else {
                continuation_width
            };
            let char_count = remaining.chars().count();

            if char_count <= limit {
                if is_first_segment {
                    items.push(ListItem::new(Line::from(vec![Span::styled(
                        remaining.to_string(),
                        style,
                    )])));
                } else {
                    items.push(ListItem::new(Line::from(vec![
                        Span::styled(continuation_indent, Style::default().fg(theme().muted)),
                        Span::styled(remaining.to_string(), style),
                    ])));
                }
                break;
            }

            // Find best wrap point at last whitespace
            let mut byte_split = 0;
            let mut last_space_byte = None;

            for (count, (b_idx, c)) in remaining.char_indices().enumerate() {
                if count == limit {
                    byte_split = b_idx;
                    break;
                }
                if c.is_whitespace() {
                    last_space_byte = Some(b_idx);
                }
            }
            if byte_split == 0 {
                byte_split = remaining.len();
            }

            let split_at = if let Some(space_idx) = last_space_byte {
                if space_idx > 0 { space_idx } else { byte_split }
            } else {
                byte_split
            };

            let (chunk, rest) = remaining.split_at(split_at);
            let rest_trimmed = rest.trim_start();

            if is_first_segment {
                items.push(ListItem::new(Line::from(vec![Span::styled(
                    chunk.to_string(),
                    style,
                )])));
                is_first_segment = false;
            } else {
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(continuation_indent, Style::default().fg(theme().muted)),
                    Span::styled(chunk.to_string(), style),
                ])));
            }

            remaining = rest_trimmed;
        }
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(vec![Span::raw("")])));
    }

    items
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(34)])
        .split(area);

    let mut spans = vec![Span::styled(
        " YuE2 Studio",
        Style::default()
            .fg(theme().info)
            .add_modifier(Modifier::BOLD),
    )];

    if let Some(balance) = app.balance {
        push_status(
            &mut spans,
            format!("${:.2}", balance),
            Style::default()
                .fg(theme().ok_bright)
                .add_modifier(Modifier::BOLD),
        );
    }

    if app.tunnel_alive {
        push_status(
            &mut spans,
            format!("● tunnel :{}", app.config.local_port),
            Style::default().fg(theme().ok).add_modifier(Modifier::BOLD),
        );
    } else if app.ssh_tunnel.is_some() {
        push_status(
            &mut spans,
            format!("✕ tunnel ({})", app.tunnel_status),
            Style::default()
                .fg(theme().danger)
                .add_modifier(Modifier::BOLD),
        );
    } else {
        push_status(
            &mut spans,
            "○ tunnel".to_string(),
            Style::default().fg(theme().muted),
        );
    }

    if app.is_server_healthy {
        push_status(
            &mut spans,
            "● server".to_string(),
            Style::default().fg(theme().ok).add_modifier(Modifier::BOLD),
        );
    } else if app.tunnel_alive {
        push_status(
            &mut spans,
            "✕ server".to_string(),
            Style::default()
                .fg(theme().danger)
                .add_modifier(Modifier::BOLD),
        );
    } else {
        push_status(
            &mut spans,
            "○ server".to_string(),
            Style::default().fg(theme().muted),
        );
    }

    if app.web_ui_process.is_some() {
        push_status(
            &mut spans,
            "● web ui".to_string(),
            Style::default().fg(theme().ok).add_modifier(Modifier::BOLD),
        );
    } else {
        push_status(
            &mut spans,
            "○ web ui".to_string(),
            Style::default().fg(theme().muted),
        );
    }

    if let Some(pod) = app.selected_pod()
        && pod.desired_status.eq_ignore_ascii_case("RUNNING")
        && let Some(uptime_secs) = pod.runtime.as_ref().and_then(|r| r.uptime_in_seconds)
    {
        let (uptime_str, uptime_style) = format_uptime(Some(uptime_secs));
        push_status(&mut spans, uptime_str, uptime_style);
    }

    if let Some(busy) = &app.busy_operation {
        push_status(
            &mut spans,
            format!("{} {}", app.spinner(), busy),
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        );
    }

    let left = Paragraph::new(Line::from(spans)).style(Style::default().bg(theme().bar_bg));
    f.render_widget(left, chunks[0]);

    let right = Line::from(vec![
        Span::styled(
            "[?]",
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" help   ", Style::default().fg(theme().text_dim)),
        Span::styled(
            "[c]",
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" config   ", Style::default().fg(theme().text_dim)),
        Span::styled(
            "[q]",
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" quit ", Style::default().fg(theme().text_dim)),
    ]);
    let right = Paragraph::new(right)
        .alignment(Alignment::Right)
        .style(Style::default().bg(theme().bar_bg));
    f.render_widget(right, chunks[1]);
}

fn push_status(spans: &mut Vec<Span<'static>>, text: String, style: Style) {
    spans.push(Span::styled(" │ ", Style::default().fg(theme().muted)));
    spans.push(Span::styled(text, style));
}

fn draw_volume_select_modal(f: &mut Frame, app: &App, selected_idx: usize) {
    let area = responsive_modal_rect(84, 20, f.area());
    f.render_widget(Clear, area);

    let block = style::modal("Step 1 of 3 · Network Volume", theme().accent);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 3 || inner.width < 10 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // List of volumes
            Constraint::Length(1), // Bottom instruction bar
        ])
        .split(inner);

    let mut items = Vec::new();

    for (idx, vol) in app.network_volumes.iter().enumerate() {
        let is_sel = idx == selected_idx;
        let dc = vol.data_center_id.as_deref().unwrap_or("Global");
        let line = Line::from(vec![
            Span::styled(style::marker(is_sel), Style::default().fg(theme().accent)),
            Span::styled(
                format!("{:<22}", vol.name),
                if is_sel {
                    Style::default()
                        .fg(theme().accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme().text)
                },
            ),
            Span::styled(
                format!("{} GB ", vol.size),
                Style::default().fg(theme().info),
            ),
            Span::styled(format!("[Region: {}]", dc), Style::default().fg(theme().ok)),
            Span::styled(
                " (mounts at /workspace)",
                Style::default().fg(theme().muted),
            ),
        ]);
        items.push(ListItem::new(line));
    }

    // Ephemeral option
    let is_ephemeral_sel = selected_idx == app.network_volumes.len();
    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            style::marker(is_ephemeral_sel),
            Style::default().fg(theme().accent),
        ),
        Span::styled(
            "No Network Volume (Ephemeral / Any Global Region)",
            if is_ephemeral_sel {
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme().muted)
            },
        ),
    ])));

    let list = List::new(items);

    f.render_widget(list, chunks[0]);

    let instr_widget = style::hints(&[
        ("↑/↓", "select"),
        ("r", "refresh"),
        ("Enter", "next: GPUs"),
        ("Esc", "cancel"),
    ]);
    f.render_widget(instr_widget, chunks[1]);
}

fn draw_gpu_select_modal(
    f: &mut Frame,
    selected_volume: Option<&crate::runpod::NetworkVolume>,
    gpus: &[crate::runpod::GpuType],
    selected_index: usize,
) {
    let area = responsive_modal_rect(96, 26, f.area());
    f.render_widget(Clear, area);

    let dc_label = selected_volume
        .and_then(|v| v.data_center_id.as_deref())
        .unwrap_or("Global");
    let vol_label = selected_volume
        .map(|v| v.name.as_str())
        .unwrap_or("Ephemeral");

    let total = gpus.len();
    let inner_height = area.height.saturating_sub(2 + 2).saturating_sub(3);
    let visible_height = (inner_height as usize).max(1);

    // Windowed scrolling calculation
    let scroll_offset = if total <= visible_height {
        0
    } else if selected_index >= visible_height {
        (selected_index + 1 - visible_height).min(total.saturating_sub(visible_height))
    } else {
        0
    };

    let end_idx = (scroll_offset + visible_height).min(total);
    let title = format!(
        "Step 2 of 3 · GPU for {} ({}) [{}-{} of {}]",
        vol_label,
        dc_label,
        if total == 0 { 0 } else { scroll_offset + 1 },
        end_idx,
        total
    );

    let block = style::modal(title, theme().accent);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 3 || inner.width < 10 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // GPU Table
            Constraint::Length(1), // Bottom helper bar
        ])
        .split(inner);

    let header_cells = [
        "GPU Model",
        "VRAM",
        "Stock Status",
        "Community",
        "Secure Cloud",
    ];
    let header = style::table_header(&header_cells);

    let rows: Vec<Row> = gpus
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
        .map(|(idx, g)| {
            let is_sel = idx == selected_index;
            let in_stock = g.is_in_region_stock();
            let name_style = if is_sel {
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD)
            } else if in_stock {
                Style::default().fg(theme().text)
            } else {
                Style::default().fg(theme().muted)
            };
            let name_cell = Cell::from(format!("{}{}", style::marker(is_sel), g.display_name))
                .style(name_style);

            let vram_cell = Cell::from(format!("{} GB", g.memory_in_gb))
                .style(Style::default().fg(theme().info));

            let (stock_text, stock_style) = if in_stock {
                (
                    "● In Stock",
                    Style::default().fg(theme().ok).add_modifier(Modifier::BOLD),
                )
            } else if g.lowest_on_demand_price().is_some() {
                ("○ Other Region", Style::default().fg(theme().muted))
            } else {
                ("✕ Out of Stock", Style::default().fg(theme().danger))
            };
            let stock_cell = Cell::from(stock_text).style(stock_style);

            let comm_price = g
                .community_price
                .map(|c| format!("${:.2}/hr", c))
                .unwrap_or_else(|| "N/A".to_string());
            let comm_cell = Cell::from(comm_price).style(if in_stock {
                Style::default().fg(theme().ok)
            } else {
                Style::default().fg(theme().muted)
            });

            let sec_price = g
                .secure_price
                .map(|s| format!("${:.2}/hr", s))
                .unwrap_or_else(|| "N/A".to_string());
            let sec_cell = Cell::from(sec_price).style(if in_stock {
                Style::default().fg(theme().ok)
            } else {
                Style::default().fg(theme().muted)
            });

            style::table_row(
                idx,
                is_sel,
                vec![name_cell, vram_cell, stock_cell, comm_cell, sec_cell],
            )
        })
        .collect();

    let widths = [
        Constraint::Min(20),    // GPU Model: dynamically expands
        Constraint::Length(8),  // VRAM: "80 GB"
        Constraint::Length(16), // Stock Status: "● In Stock"
        Constraint::Length(13), // Community: "$0.44/hr"
        Constraint::Length(14), // Secure Cloud: "$0.74/hr"
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(2);

    f.render_widget(table, chunks[0]);

    let instr_widget = style::hints(&[
        ("↑/↓ PgUp/PgDn", "scroll"),
        ("r", "refresh stock"),
        ("Enter", "next: pod name"),
        ("Esc", "back"),
    ]);
    f.render_widget(instr_widget, chunks[1]);
}

fn draw_pod_name_modal(
    f: &mut Frame,
    selected_volume: Option<&crate::runpod::NetworkVolume>,
    gpu: &crate::runpod::GpuType,
    input: &Input,
) {
    let area = responsive_modal_rect(88, 13, f.area());
    f.render_widget(Clear, area);

    let block = style::modal("Step 3 of 3 · Pod Name", theme().accent);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Specs Summary
            Constraint::Length(3), // Input Box
            Constraint::Min(1),    // Spacer
            Constraint::Length(1), // Key instructions
        ])
        .split(inner);

    let vol_name = selected_volume
        .map(|v| v.name.as_str())
        .unwrap_or("Ephemeral (No Volume)");
    let dc_name = selected_volume
        .and_then(|v| v.data_center_id.as_deref())
        .unwrap_or("Global");
    let summary_line = Line::from(vec![
        style::label("GPU"),
        Span::styled(
            &gpu.display_name,
            Style::default()
                .fg(theme().info)
                .add_modifier(Modifier::BOLD),
        ),
        style::column_gap(),
        style::label("Volume"),
        Span::styled(vol_name, Style::default().fg(theme().accent)),
        Span::styled(
            format!(" ({})", dc_name),
            Style::default().fg(theme().muted),
        ),
    ]);
    f.render_widget(Paragraph::new(summary_line), chunks[0]);

    let input_block = style::panel("Pod Display Name", theme().accent);

    let input_inner = input_block.inner(chunks[1]);
    f.render_widget(input_block, chunks[1]);

    let spans = render_input_spans(&input.value, input.cursor);
    f.render_widget(Paragraph::new(Line::from(spans)), input_inner);

    let instr_widget = style::hints(&[("type", "edit name"), ("Enter", "deploy"), ("Esc", "back")]);
    f.render_widget(instr_widget, chunks[3]);
}

fn draw_confirm_modal(
    f: &mut Frame,
    title: &str,
    border_color: Color,
    message_lines: &[&str],
    confirm_label: &str,
    cancel_label: &str,
    selected_choice: usize,
) {
    let area = responsive_modal_rect(64, 15, f.area());
    f.render_widget(Clear, area);

    let block = style::modal(title, border_color);

    let mut lines = vec![Line::from("")];
    for (idx, msg) in message_lines.iter().enumerate() {
        if idx == 0 {
            lines.push(Line::from(Span::styled(
                *msg,
                Style::default()
                    .fg(theme().text)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                *msg,
                Style::default().fg(theme().muted),
            )));
        }
    }
    lines.push(Line::from(""));

    let yes_style = if selected_choice == 0 {
        Style::default()
            .bg(border_color)
            .fg(theme().on_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(border_color)
            .add_modifier(Modifier::BOLD)
    };

    let cancel_style = if selected_choice == 1 {
        Style::default()
            .bg(theme().text)
            .fg(theme().on_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme().muted)
    };

    let yes_prefix = style::marker(selected_choice == 0);
    let cancel_prefix = style::marker(selected_choice == 1);

    lines.push(Line::from(vec![
        Span::styled(format!(" {}{}[y] ", yes_prefix, confirm_label), yes_style),
        Span::raw("    "),
        Span::styled(
            format!(" {}{} [n] ", cancel_prefix, cancel_label),
            cancel_style,
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "←/→ to focus · Enter to select · y/n to decide",
        Style::default().fg(theme().text_dim),
    )));

    let p = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center);
    f.render_widget(p, area);
}

/// Start / stop / terminate: one dialog, three sets of words.
fn draw_pod_confirm_modal(
    f: &mut Frame,
    pod_name: &str,
    action: PodAction,
    selected_choice: usize,
) {
    let (title, color, lines, label) = match action {
        PodAction::Start => (
            "Start Pod",
            theme().ok,
            vec![
                format!("Start pod '{}'?", pod_name),
                "On-demand GPU billing will resume.".to_string(),
            ],
            "Start Pod",
        ),
        PodAction::Stop => (
            "Stop Pod",
            theme().accent,
            vec![
                format!("Stop pod '{}' (release GPU)?", pod_name),
                "GPU billing will pause. Network volume data is preserved.".to_string(),
            ],
            "Stop GPU",
        ),
        PodAction::Terminate => (
            "Terminate Pod",
            theme().danger,
            vec![
                format!("Are you sure you want to terminate '{}'?", pod_name),
                "• The instance will be permanently deleted.".to_string(),
                "• Your attached Network Volume will be preserved.".to_string(),
            ],
            "Yes, Terminate",
        ),
    };
    draw_confirm_modal(
        f,
        title,
        color,
        &refs(&lines),
        label,
        "Cancel",
        selected_choice,
    );
}

fn draw_build_confirm_modal(f: &mut Frame, target: BuildTarget, selected_choice: usize) {
    let (title, lines, label) = match target {
        BuildTarget::Server => (
            "Build Server",
            vec![
                "Build release binary for music-server?".to_string(),
                "Target: crates/music-server on local machine.".to_string(),
            ],
            "Build Server",
        ),
        BuildTarget::Engine => (
            "Build Engine",
            vec![
                "Build native release binary for music-engine?".to_string(),
                "Target: crates/music-engine (with Torch & ACPP features).".to_string(),
            ],
            "Build Engine",
        ),
    };
    draw_confirm_modal(
        f,
        title,
        theme().highlight,
        &refs(&lines),
        label,
        "Cancel",
        selected_choice,
    );
}

fn refs(lines: &[String]) -> Vec<&str> {
    lines.iter().map(String::as_str).collect()
}

struct HelpEntry {
    key: &'static str,
    action: &'static str,
    desc: &'static str,
}

fn draw_help_modal(f: &mut Frame, scroll_offset: usize) {
    let area = responsive_modal_rect(108, 25, f.area());
    f.render_widget(Clear, area);

    let entries: &[HelpEntry] = &[
        HelpEntry {
            key: "Enter",
            action: "Toggle Tunnel",
            desc: "Open/close SSH tunnel to remote pod (127.0.0.1:3791)",
        },
        HelpEntry {
            key: "r",
            action: "Refresh Sync",
            desc: "Sync pods, network volumes, and GPU catalog from RunPod",
        },
        HelpEntry {
            key: "n",
            action: "Deploy Pod",
            desc: "Launch 2-step wizard (Volume & GPU selector)",
        },
        HelpEntry {
            key: "s",
            action: "Start Pod",
            desc: "Resume stopped pod instance (resumes GPU billing)",
        },
        HelpEntry {
            key: "x",
            action: "Stop Pod",
            desc: "Pause pod instance (stops GPU billing, retains storage)",
        },
        HelpEntry {
            key: "t",
            action: "Terminate Pod",
            desc: "Permanently destroy pod instance (storage preserved)",
        },
        HelpEntry {
            key: "o",
            action: "Open Web UI",
            desc: "Launch YuE2 Studio in your default browser",
        },
        HelpEntry {
            key: "w",
            action: "Toggle Vite",
            desc: "Start or stop local Vite frontend dev server",
        },
        HelpEntry {
            key: "d",
            action: "Restart Daemon",
            desc: "Restart remote studio daemon process via SSH",
        },
        HelpEntry {
            key: "b",
            action: "Build Server",
            desc: "Compile music-server Rust binary for Linux target",
        },
        HelpEntry {
            key: "e",
            action: "Build Engine",
            desc: "Compile native CUDA yue-server C++ engine",
        },
        HelpEntry {
            key: "p",
            action: "Push Binaries",
            desc: "SCP binaries to pod & trigger service restart",
        },
        HelpEntry {
            key: "/",
            action: "Filter Logs",
            desc: "Real-time interactive log search and filter in console",
        },
        HelpEntry {
            key: "c",
            action: "Configuration",
            desc: "Edit API key, ports, poll intervals, SSH key and theme",
        },
        HelpEntry {
            key: "?",
            action: "Help Guide",
            desc: "Toggle this keybindings reference modal",
        },
        HelpEntry {
            key: "q",
            action: "Quit Launcher",
            desc: "Disconnect active sessions and exit launcher",
        },
    ];

    let total = entries.len();
    let visible_rows = (area.height.saturating_sub(2 + 2 + 3) as usize).max(1);
    let offset = scroll_offset.min(total.saturating_sub(visible_rows));
    let end_idx = (offset + visible_rows).min(total);

    let title = format!(
        "Keyboard Shortcuts [{}-{} of {}]",
        if total == 0 { 0 } else { offset + 1 },
        end_idx,
        total
    );

    let block = style::modal(title, theme().accent);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 3 || inner.width < 10 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Help Table
            Constraint::Length(1), // Footer instructions
        ])
        .split(inner);

    let header = style::table_header(&["Key", "Action", "Description"]);

    let rows: Vec<Row> = entries
        .iter()
        .skip(offset)
        .take(visible_rows)
        .enumerate()
        .map(|(idx, entry)| {
            let key_cell = Cell::from(format!("[{}]", entry.key)).style(
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            );
            let action_cell = Cell::from(entry.action).style(
                Style::default()
                    .fg(theme().text)
                    .add_modifier(Modifier::BOLD),
            );
            let desc_cell = Cell::from(entry.desc).style(Style::default().fg(theme().text_dim));

            style::table_row(offset + idx, false, vec![key_cell, action_cell, desc_cell])
        })
        .collect();

    let widths = [
        Constraint::Length(12), // Key: "[Enter]"
        Constraint::Length(20), // Action: "Toggle Tunnel"
        Constraint::Min(50),    // Description
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(2);

    f.render_widget(table, chunks[0]);

    let instr_widget = style::hints(&[("↑/↓", "scroll"), ("Esc", "close")]);
    f.render_widget(instr_widget, chunks[1]);
}

fn draw_settings_modal(
    f: &mut Frame,
    app: &App,
    selected_idx: usize,
    editing: bool,
    input: &Input,
) {
    let area = responsive_modal_rect(98, 16, f.area());
    f.render_widget(Clear, area);

    let block = style::modal("Configuration", theme().accent);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 3 || inner.width < 10 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Settings Table
            Constraint::Length(1), // Bottom helper bar
        ])
        .split(inner);

    let port_str = app.config.local_port.to_string();
    let poll_str = app.config.server_poll_interval_secs.to_string();
    let refresh_str = app.config.pod_refresh_interval_secs.to_string();
    let theme_str = app.config.theme.clone();

    let fields = [
        (
            "RunPod API Key",
            app.config
                .runpod_api_key
                .as_deref()
                .unwrap_or("<Not configured>"),
        ),
        ("Local Forward Port", &port_str[..]),
        ("Server Poll (seconds)", &poll_str[..]),
        ("RunPod Auto-Refresh (seconds)", &refresh_str[..]),
        (
            "Custom SSH Key File",
            app.config
                .ssh_key_path
                .as_ref()
                .map(|p| p.to_str().unwrap_or(""))
                .unwrap_or("<Auto-detect>"),
        ),
        ("Theme", theme_str.as_str()),
    ];

    let header = style::table_header(&["Setting", "Value"]);

    let rows: Vec<Row> = fields
        .iter()
        .enumerate()
        .map(|(idx, (field, val))| {
            let is_sel = idx == selected_idx;
            let is_theme = idx == THEME_ROW;

            let label_style = if is_sel {
                Style::default()
                    .fg(theme().text)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme().text_dim)
            };
            let label_cell =
                Cell::from(format!("{}{}", style::marker(is_sel), field)).style(label_style);

            let val_cell = if is_sel && editing {
                let spans = render_input_spans(&input.value, input.cursor);
                Cell::from(Line::from(spans))
            } else if is_theme {
                Cell::from(format!("← {} →", val)).style(
                    Style::default()
                        .fg(theme().accent)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                let display_val = if idx == 0 && val.len() > 10 {
                    format!("{}...{}", &val[..4], &val[val.len() - 4..])
                } else {
                    val.to_string()
                };
                let val_style = if is_sel {
                    Style::default()
                        .fg(theme().accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme().accent)
                };
                Cell::from(display_val).style(val_style)
            };

            style::table_row(idx, is_sel, vec![label_cell, val_cell])
        })
        .collect();

    let widths = [
        Constraint::Length(34), // Setting
        Constraint::Min(40),    // Value
    ];

    let table = Table::new(rows, widths).header(header).column_spacing(2);

    f.render_widget(table, chunks[0]);

    let instr_widget = if editing {
        style::hints(&[("type", "edit value"), ("Enter", "save"), ("Esc", "cancel")])
    } else if selected_idx == 6 {
        style::hints(&[("←/→", "change theme"), ("Esc", "close")])
    } else {
        style::hints(&[("↑/↓", "select"), ("Enter", "edit"), ("Esc", "close")])
    };
    f.render_widget(instr_widget, chunks[1]);
}

fn render_input_spans(buf: &str, cursor_pos: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = buf.chars().collect();
    let before: String = chars.iter().take(cursor_pos).collect();
    let at: String = if cursor_pos < chars.len() {
        chars[cursor_pos].to_string()
    } else {
        " ".to_string()
    };
    let after: String = chars.iter().skip(cursor_pos + 1).collect();

    vec![
        Span::styled(before, Style::default().fg(theme().accent)),
        Span::styled(
            at,
            Style::default()
                .bg(theme().accent)
                .fg(theme().on_accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(after, Style::default().fg(theme().accent)),
    ]
}

fn responsive_modal_rect(target_width: u16, target_height: u16, r: Rect) -> Rect {
    // Two cells of margin on every side, so the dashboard frames the dialog
    // instead of showing slivers of its borders through the edges.
    let width = target_width.min(r.width.saturating_sub(4)).max(20);
    let height = target_height.min(r.height.saturating_sub(4)).max(8);
    let x = (r.width.saturating_sub(width)) / 2;
    let y = (r.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
