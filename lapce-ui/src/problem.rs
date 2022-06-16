use std::path::PathBuf;

use druid::{
    piet::{Text, TextLayout as PietTextLayout, TextLayoutBuilder},
    BoxConstraints, Command, Cursor, Data, Env, Event, EventCtx, LayoutCtx,
    LifeCycle, LifeCycleCtx, MouseEvent, PaintCtx, Point, RenderContext, Size,
    Target, UpdateCtx, Widget, WidgetExt, WidgetId,
};
use itertools::Itertools;
use lapce_data::{
    command::{LapceUICommand, LAPCE_UI_COMMAND},
    config::LapceTheme,
    data::{EditorDiagnostic, LapceTabData, PanelKind},
    editor::EditorLocation,
    problem::ProblemData,
    proxy::path_from_url,
    split::SplitDirection,
};
use lsp_types::{DiagnosticSeverity, Position};

use crate::{
    panel::{LapcePanel, PanelHeaderKind},
    svg::{file_svg, get_svg},
};

pub fn new_problem_panel(data: &ProblemData) -> LapcePanel {
    LapcePanel::new(
        PanelKind::Problem,
        data.widget_id,
        data.split_id,
        SplitDirection::Vertical,
        PanelHeaderKind::Simple("Problem".into()),
        vec![
            (
                data.error_widget_id,
                PanelHeaderKind::Simple("Errors".into()),
                ProblemContent::new(DiagnosticSeverity::Error).boxed(),
                None,
            ),
            (
                data.warning_widget_id,
                PanelHeaderKind::Simple("Warnings".into()),
                ProblemContent::new(DiagnosticSeverity::Warning).boxed(),
                None,
            ),
        ],
    )
}

struct ProblemContent {
    severity: DiagnosticSeverity,
    mouse_pos: Point,
    line_height: f64,
    content_height: f64,
}

impl ProblemContent {
    pub fn new(severity: DiagnosticSeverity) -> Self {
        Self {
            severity,
            mouse_pos: Point::ZERO,
            line_height: 25.0,
            content_height: 0.0,
        }
    }

    fn items<'a>(
        &self,
        data: &'a LapceTabData,
    ) -> Vec<(&'a PathBuf, Vec<&'a EditorDiagnostic>)> {
        data.main_split
            .diagnostics
            .iter()
            .filter_map(|(path, diagnostic)| {
                let diagnostics: Vec<&EditorDiagnostic> = diagnostic
                    .iter()
                    .filter(|d| d.diagnostic.severity == Some(self.severity))
                    .collect();
                if !diagnostics.is_empty() {
                    Some((path, diagnostics))
                } else {
                    None
                }
            })
            .sorted_by_key(|(path, _)| (*path).clone())
            .collect()
    }

    /// Collapse file diagnostic or skip to diagnostic.
    fn mouse_down(
        &self,
        ctx: &mut EventCtx,
        mouse_event: &MouseEvent,
        data: &LapceTabData,
    ) {
        let click_line = (mouse_event.pos.y / self.line_height).floor() as usize;
        let items = self.items(data);
        let mut line_cursor = 0;

        let mut it = items.into_iter().peekable();

        // Skip files before clicked section
        while let Some((path, diagnostics)) = it.peek() {
            let is_collapsed =
                data.problem.collapsed.get(*path).copied().unwrap_or(false);
            let offset = if is_collapsed {
                // If section is collapsed count only header with file name
                1
            } else {
                // Total file lines and header with file name
                diagnostics.iter().map(|d| d.lines).sum::<usize>() + 1 /* file name header */
            };
            // did we reached clicked section?
            if offset + line_cursor <= click_line {
                // No. Move line cursor and consume file
                line_cursor += offset;
                it.next();
            } else {
                // Current file is what we are looking for
                break;
            }
        }

        //
        let (path, diagnostics) = it.next()
            .expect("Unexpected end of editor diagnostics. We should found here currently clicked diagnostic message or file name header. This should never happen, please report a bug");

        // handle click on header with file name
        if line_cursor == click_line {
            ctx.submit_command(Command::new(
                LAPCE_UI_COMMAND,
                LapceUICommand::ToggleProblem(path.to_path_buf()),
                Target::Widget(data.id),
            ));
            return;
        }

        if data.problem.collapsed.get(path).copied().unwrap_or(false) {
            log::warn!(
                "File is collapsed. Can't click any element. This shouldn't happen, please report a bug."
            );
            return;
        }

        // Skip to clicked diagnostic
        let mut it = diagnostics.into_iter().peekable();
        while let Some(file_diagnostic) = it.peek() {
            // Is current diagnostic the clicked one?
            if line_cursor + file_diagnostic.lines < click_line {
                // No. Move line cursor and consume diagnostic
                line_cursor += file_diagnostic.lines;
                it.next();
            } else {
                // We found diagnostic we are looking for
                break;
            }
        }

        // Handle current diagnostic
        let file_diagnostic = it.next().expect("Editor diagnostic not found. We should find here file diagnostic but nothing left in the array. Please report a bug");

        if line_cursor > click_line {
            log::error!(
                "Line cursor is larger than clicked line. This should never happen!"
            );
            return;
        }

        let msg_lines = file_diagnostic.diagnostic.message.lines().count();

        // Widget has mouse about it and line is clicked one.
        if ctx.is_hot()
            && (line_cursor..(line_cursor + msg_lines)).contains(&click_line)
        {
            // rust example: description without location
            Self::submit_jump(
                ctx,
                path.to_path_buf(),
                file_diagnostic.diagnostic.range.start,
                data.id,
            );
            return;
        }
        line_cursor += msg_lines;

        // Skip to clicked related information
        let mut it = file_diagnostic
            .diagnostic
            .related_information
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .peekable();

        while let Some(related) = it.peek() {
            let lines = related.message.lines().count() + 1 /*related info will have own file name header with msg location*/;
            // is current line the clicked one?
            if (line_cursor..=(line_cursor + lines)).contains(&click_line) {
                // Yes. Do not move line cursor and stop
                break;
            }
            // No. Move line cursor and consume related info
            line_cursor += lines;
            it.next();
        }

        let related = it.next()
            .expect("No related information found but something was clicked. This should never happen. Please report a bug.");

        Self::submit_jump(
            ctx,
            related.location.uri.to_file_path().unwrap(),
            related.location.range.start,
            data.id,
        );
    }

    fn submit_jump(
        ctx: &mut EventCtx,
        path: PathBuf,
        start: Position,
        id: WidgetId,
    ) {
        ctx.submit_command(Command::new(
            LAPCE_UI_COMMAND,
            LapceUICommand::JumpToLocation(
                None,
                EditorLocation {
                    path,
                    position: Some(start),
                    scroll_offset: None,
                    history: None,
                },
            ),
            Target::Widget(id),
        ));
    }
}

impl Widget<LapceTabData> for ProblemContent {
    fn event(
        &mut self,
        ctx: &mut EventCtx,
        event: &Event,
        data: &mut LapceTabData,
        _env: &Env,
    ) {
        match event {
            Event::MouseMove(mouse_event) => {
                self.mouse_pos = mouse_event.pos;

                if mouse_event.pos.y < self.content_height {
                    ctx.set_cursor(&Cursor::Pointer);
                } else {
                    ctx.clear_cursor();
                }

                ctx.request_paint();
            }
            Event::MouseDown(mouse_event) => {
                self.mouse_down(ctx, mouse_event, data);
            }
            _ => {}
        }
    }

    fn lifecycle(
        &mut self,
        _ctx: &mut LifeCycleCtx,
        _event: &LifeCycle,
        _data: &LapceTabData,
        _env: &Env,
    ) {
    }

    fn update(
        &mut self,
        ctx: &mut UpdateCtx,
        old_data: &LapceTabData,
        data: &LapceTabData,
        _env: &Env,
    ) {
        if !data
            .main_split
            .diagnostics
            .same(&old_data.main_split.diagnostics)
        {
            ctx.request_layout();
        }
    }

    fn layout(
        &mut self,
        _ctx: &mut LayoutCtx,
        bc: &BoxConstraints,
        data: &LapceTabData,
        _env: &Env,
    ) -> Size {
        let items = self.items(data);
        let lines = items
            .iter()
            .map(|(path, diagnostics)| {
                let is_collapsed =
                    data.problem.collapsed.get(*path).copied().unwrap_or(false);
                if is_collapsed {
                    1
                } else {
                    diagnostics.iter().map(|d| d.lines).sum::<usize>() + 1 /* file name header */
                }
            })
            .sum::<usize>();
        let line_height = data.config.editor.line_height as f64;
        self.content_height = line_height * lines as f64;

        Size::new(bc.max().width, self.content_height.max(bc.max().height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, data: &LapceTabData, _env: &Env) {
        let line_height = data.config.editor.line_height as f64;
        let size = ctx.size();
        let mouse_line = (self.mouse_pos.y / line_height).floor() as usize;

        let rect = ctx.region().bounding_box();
        let min = (rect.y0 / line_height).floor() as usize;
        let max = (rect.y1 / line_height) as usize + 2;

        let items = self.items(data);
        let mut current_line = 0;
        for (path, diagnostics) in items {
            let is_collapsed =
                data.problem.collapsed.get(path).copied().unwrap_or(false);
            let diagnostics_len = diagnostics.iter().map(|d| d.lines).sum::<usize>();
            if !is_collapsed && diagnostics_len + 1 + current_line < min {
                current_line += diagnostics_len + 1;
                continue;
            }

            let padding = (line_height - 14.0) / 2.0;
            let svg = file_svg(path);
            let rect = Size::new(line_height, line_height)
                .to_rect()
                .with_origin(Point::new(0.0, line_height * current_line as f64))
                .inflate(-padding, -padding);
            ctx.draw_svg(&svg, rect, None);

            let text_layout = ctx
                .text()
                .new_text_layout(
                    path.file_name().unwrap().to_str().unwrap().to_string(),
                )
                .font(
                    data.config.ui.font_family(),
                    data.config.ui.font_size() as f64,
                )
                .text_color(
                    data.config
                        .get_color_unchecked(LapceTheme::EDITOR_FOREGROUND)
                        .clone(),
                )
                .build()
                .unwrap();
            ctx.draw_text(
                &text_layout,
                Point::new(
                    line_height,
                    line_height * current_line as f64
                        + (line_height - text_layout.size().height) / 2.0,
                ),
            );

            if is_collapsed {
                current_line += 1;
                continue;
            }

            let mut path = path.clone();
            if let Some(workspace_path) = data.workspace.path.as_ref() {
                path = path
                    .strip_prefix(workspace_path)
                    .unwrap_or(&path)
                    .to_path_buf();
            }
            let folder = path
                .parent()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if !folder.is_empty() {
                let x = text_layout.size().width + line_height + 5.0;

                let text_layout = ctx
                    .text()
                    .new_text_layout(folder)
                    .font(
                        data.config.ui.font_family(),
                        data.config.ui.font_size() as f64,
                    )
                    .text_color(
                        data.config
                            .get_color_unchecked(LapceTheme::EDITOR_DIM)
                            .clone(),
                    )
                    .build()
                    .unwrap();
                ctx.draw_text(
                    &text_layout,
                    Point::new(
                        x,
                        line_height * current_line as f64
                            + (line_height - text_layout.size().height) / 2.0,
                    ),
                );
            }

            for d in diagnostics {
                if current_line > max {
                    return;
                }
                let msg_lines = d.diagnostic.message.lines().count();
                let related_lines = d
                    .diagnostic
                    .related_information
                    .as_ref()
                    .map(|r| r.iter().map(|r| r.message.lines().count() + 1/* file name and location header */).sum())
                    .unwrap_or(0);
                if current_line + 1 + msg_lines + related_lines < min {
                    current_line += msg_lines + related_lines;
                    continue;
                }

                if ctx.is_hot()
                    && current_line < mouse_line
                    && mouse_line < current_line + 1 + msg_lines
                {
                    ctx.fill(
                        Size::new(size.width, line_height * msg_lines as f64)
                            .to_rect()
                            .with_origin(Point::new(
                                0.0,
                                line_height * (current_line + 1) as f64,
                            )),
                        data.config
                            .get_color_unchecked(LapceTheme::EDITOR_CURRENT_LINE),
                    );
                }

                let svg = match self.severity {
                    DiagnosticSeverity::Error => get_svg("error.svg").unwrap(),
                    _ => get_svg("warning.svg").unwrap(),
                };
                let rect = Size::new(line_height, line_height)
                    .to_rect()
                    .with_origin(Point::new(
                        line_height,
                        line_height * (current_line + 1) as f64,
                    ))
                    .inflate(-padding, -padding);
                ctx.draw_svg(
                    &svg,
                    rect,
                    Some(
                        data.config
                            .get_color_unchecked(LapceTheme::EDITOR_FOREGROUND),
                    ),
                );

                for line in d.diagnostic.message.lines() {
                    current_line += 1;
                    let text_layout = ctx
                        .text()
                        .new_text_layout(line.to_string())
                        .font(
                            data.config.ui.font_family(),
                            data.config.ui.font_size() as f64,
                        )
                        .text_color(
                            data.config
                                .get_color_unchecked(LapceTheme::EDITOR_FOREGROUND)
                                .clone(),
                        )
                        .build()
                        .unwrap();
                    ctx.draw_text(
                        &text_layout,
                        Point::new(
                            2.0 * line_height,
                            line_height * current_line as f64
                                + (line_height - text_layout.size().height) / 2.0,
                        ),
                    );
                }

                for related in
                    d.diagnostic.related_information.as_deref().unwrap_or(&[])
                {
                    current_line += 1;

                    if ctx.is_hot() && mouse_line >= current_line {
                        let lines = related.message.lines().count() + 1;
                        if mouse_line < current_line + lines {
                            ctx.fill(
                                Size::new(size.width, line_height * lines as f64)
                                    .to_rect()
                                    .with_origin(Point::new(
                                        0.0,
                                        line_height * current_line as f64,
                                    )),
                                data.config.get_color_unchecked(
                                    LapceTheme::EDITOR_CURRENT_LINE,
                                ),
                            );
                        }
                    }

                    let svg = get_svg("link.svg").unwrap();
                    let rect = Size::new(line_height, line_height)
                        .to_rect()
                        .with_origin(Point::new(
                            2.0 * line_height,
                            line_height * current_line as f64,
                        ))
                        .inflate(-padding, -padding);
                    ctx.draw_svg(
                        &svg,
                        rect,
                        Some(
                            data.config
                                .get_color_unchecked(LapceTheme::EDITOR_FOREGROUND),
                        ),
                    );
                    let path = path_from_url(&related.location.uri);
                    let text = format!(
                        "{}[{}, {}]:",
                        path.file_name().and_then(|f| f.to_str()).unwrap_or(""),
                        related.location.range.start.line,
                        related.location.range.start.character,
                    );
                    let text_layout = ctx
                        .text()
                        .new_text_layout(text)
                        .font(
                            data.config.ui.font_family(),
                            data.config.ui.font_size() as f64,
                        )
                        .text_color(
                            data.config
                                .get_color_unchecked(LapceTheme::EDITOR_DIM)
                                .clone(),
                        )
                        .build()
                        .unwrap();
                    ctx.draw_text(
                        &text_layout,
                        Point::new(
                            3.0 * line_height,
                            line_height * current_line as f64
                                + (line_height - text_layout.size().height) / 2.0,
                        ),
                    );
                    for line in related.message.lines() {
                        current_line += 1;

                        let text_layout = ctx
                            .text()
                            .new_text_layout(line.to_string())
                            .font(
                                data.config.ui.font_family(),
                                data.config.ui.font_size() as f64,
                            )
                            .text_color(
                                data.config
                                    .get_color_unchecked(LapceTheme::EDITOR_DIM)
                                    .clone(),
                            )
                            .build()
                            .unwrap();
                        ctx.draw_text(
                            &text_layout,
                            Point::new(
                                3.0 * line_height,
                                line_height * current_line as f64
                                    + (line_height - text_layout.size().height)
                                        / 2.0,
                            ),
                        );
                    }
                }
            }
            current_line += 1;
        }
    }
}
