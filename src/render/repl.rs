use super::{MarkdownRender, ReplyEvent};

use crate::utils::{split_line_tail, AbortSignal};

use anyhow::Result;
use crossbeam::channel::Receiver;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    queue, style,
    terminal::{self, disable_raw_mode, enable_raw_mode},
};
use std::{
    io::{self, Stdout, Write},
    time::{Duration, Instant},
};
use textwrap::core::display_width;

pub fn repl_render_stream(
    rx: &Receiver<ReplyEvent>,
    render: &mut MarkdownRender,
    abort: &AbortSignal,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();

    let ret = repl_render_stream_inner(rx, render, abort, &mut stdout);

    disable_raw_mode()?;

    ret
}

fn repl_render_stream_inner(
    rx: &Receiver<ReplyEvent>,
    render: &mut MarkdownRender,
    abort: &AbortSignal,
    writer: &mut Stdout,
) -> Result<()> {
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(50);

    let mut buffer = String::new();
    let mut buffer_rows = 1;

    let columns = terminal::size()?.0;

    loop {
        if abort.aborted() {
            return Ok(());
        }

        if let Ok(evt) = rx.try_recv() {
            match evt {
                ReplyEvent::Text(text) => {
                    let (col, mut row) = cursor::position()?;

                    // Fix unexpected duplicate lines on kitty, see https://github.com/sigoden/aichat/issues/105
                    if col == 0 && row > 0 && display_width(&buffer) == columns as usize {
                        row -= 1;
                    }

                    if row + 1 >= buffer_rows {
                        queue!(writer, cursor::MoveTo(0, row + 1 - buffer_rows),)?;
                    } else {
                        let scroll_rows = buffer_rows - row - 1;
                        queue!(
                            writer,
                            terminal::ScrollUp(scroll_rows),
                            cursor::MoveTo(0, 0),
                        )?;
                    }
                    queue!(writer, terminal::Clear(terminal::ClearType::UntilNewLine))?;

                    if text.contains('\n') {
                        let text = format!("{buffer}{text}");
                        let (head, tail) = split_line_tail(&text);
                        buffer = tail.to_string();
                        let output = render.render(head);
                        print_block(writer, &output, columns)?;
                        queue!(writer, style::Print(&buffer),)?;
                        buffer_rows = need_rows(&buffer, columns);
                    } else {
                        buffer = format!("{buffer}{text}");
                        let output = render.render_line(&buffer);
                        if output.contains('\n') {
                            let (head, tail) = split_line_tail(&output);
                            buffer_rows = print_block(writer, head, columns)?;
                            queue!(writer, style::Print(&tail),)?;
                            buffer_rows += need_rows(tail, columns);
                        } else {
                            queue!(writer, style::Print(&output))?;
                            buffer_rows = need_rows(&output, columns);
                        }
                    }

                    writer.flush()?;
                }
                ReplyEvent::Done => {
                    #[cfg(target_os = "windows")]
                    let eol = "\n\n";
                    #[cfg(not(target_os = "windows"))]
                    let eol = "\n";
                    queue!(writer, style::Print(eol))?;
                    writer.flush()?;

                    break;
                }
            }
            continue;
        }

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));
        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                        abort.set_ctrlc();
                        return Ok(());
                    }
                    KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
                        abort.set_ctrld();
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
    Ok(())
}

fn print_block(writer: &mut Stdout, text: &str, columns: u16) -> Result<u16> {
    let mut num = 0;
    for line in text.split('\n') {
        queue!(
            writer,
            style::Print(line),
            style::Print("\n"),
            cursor::MoveLeft(columns),
        )?;
        num += 1;
    }
    Ok(num)
}

fn need_rows(text: &str, columns: u16) -> u16 {
    let buffer_width = display_width(text) as u16;
    (buffer_width + columns - 1) / columns
}
