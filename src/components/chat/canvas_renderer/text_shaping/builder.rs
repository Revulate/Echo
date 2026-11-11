//! The incremental line builder every layout is assembled with.

use super::*;

/// Type of the last token processed (for spacing decisions)
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum LastTokenType {
    /// No token processed yet or at start of content
    None,
    /// Last token was text (including space)
    Text,
    /// Last token was an emote
    Emote,
    /// Last token was a URL, mention, or embed link
    Link,
}

pub(super) struct PendingLayeredImage {
    pub(super) emote: LayeredEmote,
    pub(super) x: f32,
    pub(super) image_type: ImageType,
}

/// Helper for building layout incrementally with dynamic line heights
pub(super) struct LayoutBuilder<'a> {
    /// Right boundary for content (absolute X position)
    pub(super) right_boundary: f32,
    /// Current X position
    pub(super) current_x: f32,
    /// Current Y position (start of current line)
    pub(super) current_y: f32,
    /// Current line index
    pub(super) current_line: u32,

    /// Accumulated text for current segment
    pub(super) pending_text: String,
    /// Number of chars in pending_text (Unicode scalar values)
    /// Avoids rescanning pending_text on flush.
    pub(super) pending_char_count: usize,
    /// Text runs for current segment
    pub(super) pending_runs: SmallVec<[TextRunData; 4]>,
    /// Start X of current text segment
    pub(super) text_segment_start_x: f32,
    /// Exact 7TV paint spec for the username inside the current text segment.
    pub(super) pending_seventv_paint: Option<std::sync::Arc<SevenTvLinePaint>>,
    /// Character positions for current segment (built during add_text_run, reused in flush)
    pub(super) pending_char_positions: SmallVec<[f32; 24]>,

    /// X position where message content starts (after timestamp/badges/username)
    /// Used for indenting wrapped lines
    pub(super) content_start_x: f32,
    /// Whether we've passed the username (content has started)
    pub(super) content_started: bool,
    /// Whether timestamps are shown (affects wrap indent calculation)
    pub(super) show_timestamp: bool,

    /// All completed segments (from committed lines)
    pub(super) segments: SmallVec<[LayoutSegment; 8]>,
    /// All shaped line data
    pub(super) shaped_lines: SmallVec<[ShapedLineData; 4]>,
    /// All hitbox regions
    /// Sized for the hitbox count of a typical message.
    /// (badges, username, mentions, emotes, URLs) so 16 avoids heap allocation
    pub(super) hitboxes: SmallVec<[HitboxRegion; 16]>,
    /// Mention highlight regions (x, y, width, height)
    pub(super) mention_highlights: SmallVec<[(f32, f32, f32, f32); 4]>,

    /// Selection map line offsets
    pub(super) line_offsets: Vec<usize>,
    /// Total source text length in characters (Unicode scalar values)
    pub(super) source_offset: usize,
    /// Absolute X positions for each character (for selection highlighting)
    pub(super) source_char_positions: SmallVec<[f32; 48]>,
    /// Source text for clipboard (built alongside positions for consistency)
    pub(super) source_text: String,

    /// Twitch/BTTV URLs that did not have decoded dimensions when this layout
    /// was built. They are transferred to the layout for a targeted refresh
    /// when cache residency later makes native geometry available.
    pub(super) pending_native_dimension_urls: SmallVec<[SharedString; 4]>,
    pub(super) reserved_badge_channel: Option<std::sync::Arc<str>>,

    /// Index of the last emote in the current line buffer (for zero-width overlay attachment)
    pub(super) last_emote_buffer_idx: Option<usize>,
    /// Height of the most recent media-like token, used to scale visual gaps around large emotes.
    pub(super) last_media_height: Option<f32>,

    /// Pending layered emote being built (for centering zero-width overlays)
    /// When a regular emote is encountered, a LayeredEmote is created here.
    /// Zero-width emotes are added as overlays to this pending emote.
    /// The pending emote is flushed when we encounter non-emote content or wrap/finalize.
    pub(super) pending_layered_emote: Option<PendingLayeredImage>,

    /// Type of the last token processed (for spacing decisions between tokens)
    pub(super) last_token_type: LastTokenType,
    /// Whether the most recently added text run ended with whitespace.
    /// Used to preserve text-like spacing at token boundaries (e.g. text + URL).
    pub(super) last_text_ended_with_whitespace: bool,

    /// Buffer for current line items (committed on wrap or finalize)
    pub(super) line_buffer: LineBuffer,

    /// Y positions for each committed line (for selection hit testing)
    pub(super) line_y_positions: SmallVec<[f32; 8]>,

    /// Index where hitboxes for current line start (for Y adjustment after commit)
    pub(super) line_hitbox_start_idx: usize,
    /// Index where mention highlights for current line start (for Y adjustment after commit)
    pub(super) line_mention_highlight_start_idx: usize,

    /// Optional window for accurate GPUI text measurement (pixel-perfect when available)
    pub(super) window: Option<&'a mut Window>,

    /// Whether message content text should be struck through.
    pub(super) strike_content: bool,

    /// Measure and paint subsequent text runs with the ASCII-art monospace
    /// family. Enabled only after the timestamp/badges/username header so the
    /// header retains its normal UI typography.
    pub(super) monospace_text: bool,

    /// Count of animated emotes encountered during layout building
    /// Tracked here, transferred to layout in finalize() for render optimization
    pub(super) animated_emote_count: u8,

    /// Line_height() cached for the lifetime of this build (2 Mutex locks per call).
    /// Settings changes force a full relayout via cache invalidation, so this is invariant.
    pub(super) cached_line_height: f32,
    /// Where content starts after the timestamp column (and any mod buttons
    /// before it); wrapped lines indent here when timestamps are shown.
    pub(super) timestamp_column_end_x: f32,
}

impl<'a> LayoutBuilder<'a> {
    pub(super) fn new_with_y_offset(
        right_boundary: f32,
        show_timestamp: bool,
        y_offset: f32,
        mut window: Option<&'a mut Window>,
        strike_content: bool,
    ) -> Self {
        let initial_y = MESSAGE_PADDING_VERTICAL + y_offset;
        let mut line_y_positions = SmallVec::new();
        line_y_positions.push(initial_y);

        let cached_line_height = line_height();
        if let Some(win) = window.as_deref_mut() {
            crate::components::chat::canvas_renderer::layout::refresh_timestamp_width_probe(win);
        }
        let cached_timestamp_width = timestamp_width();

        Self {
            right_boundary,
            current_x: MESSAGE_PADDING_LEFT,
            current_y: initial_y,
            current_line: 0,
            pending_text: String::new(),
            pending_char_count: 0,
            pending_runs: SmallVec::new(),
            text_segment_start_x: MESSAGE_PADDING_LEFT,
            pending_seventv_paint: None,
            pending_char_positions: SmallVec::new(),
            content_start_x: MESSAGE_PADDING_LEFT,
            content_started: false,
            show_timestamp,
            segments: SmallVec::new(),
            shaped_lines: SmallVec::new(),
            hitboxes: SmallVec::new(),
            mention_highlights: SmallVec::new(),
            line_offsets: vec![0],
            source_offset: 0,
            source_char_positions: SmallVec::new(),
            source_text: String::new(),
            pending_native_dimension_urls: SmallVec::new(),
            reserved_badge_channel: None,
            last_emote_buffer_idx: None,
            last_media_height: None,
            pending_layered_emote: None,
            last_token_type: LastTokenType::None,
            last_text_ended_with_whitespace: false,
            line_buffer: LineBuffer::new(cached_line_height),
            line_y_positions,
            line_hitbox_start_idx: 0,
            line_mention_highlight_start_idx: 0,
            window,
            strike_content,
            monospace_text: false,
            animated_emote_count: 0,
            cached_line_height,
            timestamp_column_end_x: MESSAGE_PADDING_LEFT + cached_timestamp_width + TIMESTAMP_GAP,
        }
    }

    /// Mark the start of message content (after username)
    /// Wrapped lines will indent to this position
    pub(super) fn mark_content_start(&mut self) {
        self.content_start_x = self.current_x;
        self.content_started = true;
    }

    #[inline]
    pub(super) fn track_pending_native_dimension_url(&mut self, url: &str) {
        if !self
            .pending_native_dimension_urls
            .iter()
            .any(|pending| pending.as_ref() == url)
        {
            self.pending_native_dimension_urls
                .push(SharedString::from(url));
        }
    }

    /// Measure text width accurately using GPUI when window is available, else use cached metrics
    pub(super) fn measure_text_width(&mut self, text: &str, is_bold: bool) -> f32 {
        if self.monospace_text {
            if let Some(window) = self.window.as_mut() {
                let run = TextRun {
                    len: text.len(),
                    font: Font {
                        family: text_run_intern::mono_font_family(),
                        weight: if is_bold {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        },
                        ..Default::default()
                    },
                    color: colors::text_primary(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                return window
                    .text_system()
                    .shape_line(
                        SharedString::from(text),
                        px(scaled_font_size()),
                        std::slice::from_ref(&run),
                        None,
                    )
                    .width()
                    .as_f32();
            }

            return text.chars().count() as f32 * scaled_font_size() * 0.6;
        }
        if let Some(window) = self.window.as_mut() {
            font_metrics().measure_string_gpui(text, is_bold, window)
        } else {
            font_metrics().measure_string(text, is_bold)
        }
    }

    /// Flush pending layered emote to line buffer
    ///
    /// Call this before adding non-emote content, wrapping, or finalizing.
    /// Converts the pending LayeredEmote with all its overlays into a LineItem.
    pub(super) fn flush_layered_emote(&mut self) {
        if let Some(PendingLayeredImage {
            mut emote,
            x,
            image_type,
        }) = self.pending_layered_emote.take()
        {
            if emote.base.is_animated {
                self.animated_emote_count = self.animated_emote_count.saturating_add(1);
            }
            for overlay in &emote.overlays {
                if overlay.is_animated {
                    self.animated_emote_count = self.animated_emote_count.saturating_add(1);
                }
            }

            let (bbox_width, bbox_height) = emote.bounds();
            let overflow_width = (bbox_width - emote.base.width).max(0.0);
            if overflow_width > 0.0 {
                self.current_x += overflow_width;
                self.text_segment_start_x = self.current_x;
            }

            let (source_start_char, source_len_chars) =
                self.append_source_emote_code(emote.base.name.as_ref());

            self.line_buffer.add_layered_emote(
                emote,
                x,
                image_type,
                source_start_char,
                source_len_chars,
            );
            self.last_emote_buffer_idx = Some(self.line_buffer.len() - 1);
            self.last_media_height = Some(bbox_height);
        }
    }

    /// Splice a single separating space into `source_text`/`source_char_positions`
    /// if the content appended so far didn't already end in whitespace (and
    /// there's something to separate from at all -- never insert a leading
    /// space at the very start of the message).
    ///
    /// Two call sites need this, both working around the fact that
    /// `add_text_run_weighted` appends its text verbatim with no gap logic of
    /// its own, mirroring what the *visual* spacing functions
    /// (`advance_before_emote_like` / `advance_before_text_like`) already do for
    /// `current_x`:
    /// - `append_source_emote_code`, before splicing in an emote's code (e.g.
    ///   text "word" + emote -> "word Kappa", and back-to-back emotes ->
    ///   "Kappa Kappa" rather than "KappaKappa").
    /// - the `SlotRenderUnit::Text` handler, before appending text that
    ///   immediately follows an emote: that handler deliberately strips the
    ///   text's authored leading whitespace (relying on the synthesized visual
    ///   gap instead), so without this call `source_text` would glue the
    ///   emote's just-spliced code directly onto the next word.
    ///
    /// Position values pushed here are not meaningful for x-lookup (emotes are
    /// hit-tested via their own segment geometry, not a char-position binary
    /// search) -- they only need to exist so `source_char_positions.len()` keeps
    /// tracking `source_text.chars().count()`, the invariant `source_char_len()`
    /// and every range computation depend on.
    pub(super) fn ensure_source_separator(&mut self) {
        if !self.last_text_ended_with_whitespace && !self.source_text.is_empty() {
            self.source_char_positions.push(self.current_x);
            self.source_text.push(' ');
            self.source_offset += 1;
            self.last_text_ended_with_whitespace = true;
        }
    }

    /// Append exact source text whose rendered object begins at `current_x`.
    ///
    /// Unicode emoji can contain several scalar values while occupying one image
    /// hitbox. Give each scalar the same boundary position so source ranges stay
    /// exact without allowing pointer selection to split the rendered image.
    pub(super) fn append_source_text_at_current_x(&mut self, text: &str) -> (usize, usize) {
        let start_char = self.source_offset;
        let mut len_chars = 0usize;
        for ch in text.chars() {
            self.source_char_positions.push(self.current_x);
            self.source_text.push(ch);
            len_chars += 1;
        }
        self.source_offset += len_chars;
        if let Some(last) = text.chars().last() {
            self.last_text_ended_with_whitespace = last.is_whitespace();
        }

        (start_char, len_chars)
    }

    /// Append an emote's text code into the flat `source_text`/`source_char_positions`
    /// buffers so drag-selection and Ctrl+C copy include emote codes, mirroring how
    /// `add_text_run_weighted` builds these buffers for real text runs. See
    /// `ensure_source_separator` for the separator semantics.
    ///
    /// Clears `last_text_ended_with_whitespace` afterward (codes never end in
    /// whitespace) so a following emote or text run inserts its own separator
    /// in turn.
    ///
    /// Returns `(source_start_char, source_len_chars)` covering just the code
    /// itself; the synthesized separator (if any) is excluded from the range.
    pub(super) fn append_source_emote_code(&mut self, code: &str) -> (usize, usize) {
        if code.is_empty() {
            return (self.source_offset, 0);
        }

        self.ensure_source_separator();
        self.append_source_text_at_current_x(code)
    }

    /// Add a text run to the current segment
    pub(super) fn add_text_run(&mut self, text: &str, color: Hsla, bold: bool, underline: bool) {
        self.add_text_run_weighted(text, color, bold, underline, None);
    }

    pub(super) fn add_text_run_weighted(
        &mut self,
        text: &str,
        color: Hsla,
        bold: bool,
        underline: bool,
        font_weight: Option<FontWeight>,
    ) {
        if text.is_empty() {
            return;
        }

        if self.pending_text.is_empty() {
            self.text_segment_start_x = self.current_x;
        }

        let len = text.len();
        let mut run = TextRunData::new(len, color);
        if self.monospace_text {
            run.font_family = text_run_intern::mono_font_family();
        }
        if bold {
            run = run.bold();
        }
        if let Some(font_weight) = font_weight {
            run = run.with_font_weight(font_weight);
        }
        if underline {
            run = run.underlined();
        }
        if self.strike_content && self.content_started {
            run = run.struck();
        }

        let measure_bold = font_weight.map_or(bold, |weight| uses_bold_metrics(Some(weight)));

        let start_x = self.current_x;
        let pending_before = self.pending_char_positions.len();
        self.current_x = font_metrics().batch_char_positions_into(
            text,
            start_x,
            measure_bold,
            &mut self.source_char_positions,
            &mut self.pending_char_positions,
            &mut self.source_text,
        );
        let char_count = self
            .pending_char_positions
            .len()
            .saturating_sub(pending_before);

        if self.window.is_some() && char_count > 0 {
            let source_start = self.source_char_positions.len().saturating_sub(char_count);
            let has_precise_budget = char_count <= PRECISE_CHAR_POSITION_MAX;

            if has_precise_budget {
                let font_size = scaled_font_size();
                let run = TextRun {
                    len: text.len(),
                    font: Font {
                        family: if self.monospace_text {
                            text_run_intern::mono_font_family()
                        } else {
                            text_run_intern::system_font_family()
                        },
                        weight: if measure_bold {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        },
                        ..Default::default()
                    },
                    color: colors::text_primary(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let Some(window) = self.window.as_mut() else {
                    debug_assert!(
                        false,
                        "add_text_run_weighted: window was None inside is_some() guard"
                    );
                    return;
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(text),
                    px(font_size),
                    std::slice::from_ref(&run),
                    None,
                );

                for (i, (byte_idx, _)) in text.char_indices().enumerate() {
                    let x = start_x + shaped.x_for_index(byte_idx).as_f32();
                    self.pending_char_positions[pending_before + i] = x;
                    self.source_char_positions[source_start + i] = x;
                }
                self.current_x = start_x + shaped.width().as_f32();
            } else {
                let accurate_width = self.measure_text_width(text, measure_bold);
                let estimated_width = (self.current_x - start_x).max(0.0);
                if estimated_width > f32::EPSILON {
                    let scale = accurate_width / estimated_width;
                    for i in 0..char_count {
                        let adjusted = start_x
                            + (self.pending_char_positions[pending_before + i] - start_x) * scale;
                        self.pending_char_positions[pending_before + i] = adjusted;
                        self.source_char_positions[source_start + i] = adjusted;
                    }
                } else {
                    for i in 0..char_count {
                        self.pending_char_positions[pending_before + i] = start_x;
                        self.source_char_positions[source_start + i] = start_x;
                    }
                }
                self.current_x = start_x + accurate_width;
            }
        }

        self.pending_text.push_str(text);
        self.pending_runs.push(run);
        self.pending_char_count += char_count;
        self.source_offset += char_count;
        self.last_text_ended_with_whitespace = text.chars().last().is_some_and(char::is_whitespace);
    }

    /// Add an image segment to the line buffer
    pub(super) fn add_image(
        &mut self,
        url: &str,
        width: f32,
        height: f32,
        is_animated: bool,
        image_type: ImageType,
        source_range: Option<(usize, usize)>,
    ) {
        self.flush_text_segment();

        if is_animated {
            self.animated_emote_count = self.animated_emote_count.saturating_add(1);
        }

        let url_owned = SharedString::from(url);
        self.line_buffer.add_image(
            url_owned,
            self.current_x,
            width,
            height,
            is_animated,
            image_type,
            source_range,
        );

        self.last_emote_buffer_idx = self.line_buffer.last_image_index();
        self.last_media_height = Some(height);
    }

    /// Add an inline box (like NEW badge) to the line buffer
    pub(super) fn add_inline_box(
        &mut self,
        text: &str,
        text_color: Hsla,
        px_h: Pixels,
        _px_v: Pixels,
    ) {
        self.flush_text_segment();

        let text_width = self.measure_text_width(text, false);
        let px_h_f32: f32 = px_h.into();
        let box_width = text_width + px_h_f32 * 2.0;
        let box_height = self.cached_line_height * 0.8;

        let mut bg_color = text_color;
        bg_color.a = 0.15;

        let text_owned = SharedString::from(text);
        self.line_buffer.add_inline_box(
            text_owned,
            self.current_x,
            box_width,
            box_height,
            bg_color,
            text_color,
            11.0 * settings::get_font_scale(),
        );

        self.current_x += box_width + badge_gap();
        self.text_segment_start_x = self.current_x;
    }

    /// Inline moderation button: a chip outside the selectable text with its
    /// own hitbox, placed before the timestamp.
    pub(super) fn add_mod_button(&mut self, label: &str, command: &str, destructive: bool) {
        self.flush_text_segment();
        let chip_width = self.measure_text_width(label, false) + 3.0 * 2.0;
        if self.current_x > MESSAGE_PADDING_LEFT
            && self.current_x + chip_width > self.right_boundary
        {
            self.wrap_line();
        }
        let start_x = self.current_x;
        let color = if destructive {
            colors::error()
        } else {
            colors::text_muted()
        };
        self.add_inline_box(label, color, px(3.0), px(1.0));
        let width = (self.current_x - badge_gap() - start_x).max(1.0);
        self.hitboxes.push(HitboxRegion {
            bounds: Bounds::new(
                point(px(start_x), px(self.current_y)),
                size(px(width), px(self.cached_line_height)),
            ),
            kind: HitboxKind::ModAction {
                label: SharedString::from(label.to_string()),
                command: SharedString::from(command.to_string()),
            },
        });
    }

    /// Add a hitbox at current position
    pub(super) fn add_hitbox(&mut self, kind: HitboxKind) {
        let text_width = self.current_x - self.text_segment_start_x;
        if text_width > 0.0 {
            self.hitboxes.push(HitboxRegion {
                bounds: Bounds::new(
                    point(px(self.text_segment_start_x), px(self.current_y)),
                    size(px(text_width), px(self.cached_line_height)),
                ),
                kind,
            });
        }
    }

    /// Add a hitbox GUARANTEED - even if text width is 0, creates a minimum clickable area
    /// This is critical for usernames and mentions to always be clickable
    pub(super) fn add_hitbox_guaranteed(&mut self, kind: HitboxKind) {
        let text_width = self.current_x - self.text_segment_start_x;
        let effective_width = text_width.max(8.0);

        self.hitboxes.push(HitboxRegion {
            bounds: Bounds::new(
                point(px(self.text_segment_start_x), px(self.current_y)),
                size(px(effective_width), px(self.cached_line_height)),
            ),
            kind,
        });
    }

    /// Advance X position (for fixed-width elements)
    pub(super) fn advance(&mut self, width: f32) {
        self.current_x = self.current_x.max(self.text_segment_start_x + width);
        self.text_segment_start_x = self.current_x;
    }

    /// Advance X position by a relative amount
    pub(super) fn advance_by(&mut self, amount: f32) {
        self.current_x += amount;
        self.text_segment_start_x = self.current_x;
    }

    /// Set absolute X position (for fixed-width columns like timestamp)
    pub(super) fn set_x(&mut self, x: f32) {
        self.current_x = x;
        self.text_segment_start_x = x;
    }

    /// Add mention highlight background
    pub(super) fn add_mention_highlight(&mut self, x: f32, y: f32, width: f32) {
        self.mention_highlights
            .push((x, y, width, self.cached_line_height));
    }

    /// Flush pending text into the line buffer
    /// Returns true if text was actually flushed, false if buffer was empty
    pub(super) fn flush_text_segment(&mut self) -> bool {
        if self.pending_text.is_empty() {
            return false;
        }

        let line_idx = self.shaped_lines.len();
        let seg_len_chars = self.pending_char_count;
        let seg_start_char = self.source_offset.saturating_sub(seg_len_chars);
        let width = self.current_x - self.text_segment_start_x;

        let mut char_positions = std::mem::take(&mut self.pending_char_positions);
        char_positions.push(self.current_x);

        let text: SharedString = std::mem::take(&mut self.pending_text).into();
        let runs = std::mem::take(&mut self.pending_runs);

        let mut gpui_runs: SmallVec<[TextRun; 4]> = SmallVec::with_capacity(runs.len().max(1));
        for run in &runs {
            gpui_runs.push(text_run_intern::gpui_run_from_data(run));
        }
        if gpui_runs.is_empty() {
            gpui_runs.push(text_run_intern::default_gpui_run(
                text.len(),
                colors::text_primary(),
            ));
        }

        let bidi = BidiAnalysis::new(text.as_ref());
        let (visual_char_positions, visual_to_logical) = if bidi.needs_reordering {
            (
                bidi.build_visual_char_positions(&char_positions, seg_len_chars)
                    .into_vec(),
                bidi.visual_to_logical.into_vec(),
            )
        } else {
            (Vec::new(), Vec::new())
        };

        self.shaped_lines.push(ShapedLineData {
            text,
            gpui_runs_hash: hash_gpui_text_runs(&gpui_runs),
            gpui_runs,
            char_positions,
            visual_char_positions,
            visual_to_logical,
            source_start_char: seg_start_char,
            source_len_chars: seg_len_chars,
            seventv_paint: self.pending_seventv_paint.take(),
        });

        self.line_buffer
            .add_text(line_idx, self.text_segment_start_x, width);

        self.text_segment_start_x = self.current_x;
        self.pending_char_count = 0;
        true
    }

    /// Commit the current line buffer to final segments.
    ///
    /// This finalizes the line, calculating the actual line height based on
    /// the tallest element and positioning all items with proper baseline alignment.
    pub(super) fn commit_line(&mut self) {
        if self.line_buffer.is_empty() {
            return;
        }

        let line_buffer = std::mem::replace(
            &mut self.line_buffer,
            LineBuffer::new(self.cached_line_height),
        );
        let (new_segments, actual_height) = line_buffer.finalize(self.current_y);

        let y_adjustment = actual_height - self.cached_line_height;
        if y_adjustment > 0.0 {
            for hitbox in self.hitboxes[self.line_hitbox_start_idx..].iter_mut() {
                match &hitbox.kind {
                    HitboxKind::Username { .. }
                    | HitboxKind::Url { .. }
                    | HitboxKind::Mention { .. }
                    | HitboxKind::Emote { .. } => {
                        let old_y: f32 = hitbox.bounds.origin.y.into();
                        hitbox.bounds.origin.y = px(old_y + y_adjustment);
                    }
                    _ => {}
                }
            }

            for (_, y, _, _) in
                self.mention_highlights[self.line_mention_highlight_start_idx..].iter_mut()
            {
                *y += y_adjustment;
            }
        }

        for segment in &new_segments {
            match segment {
                LayoutSegment::Image {
                    url,
                    x,
                    y,
                    width,
                    height,
                    image_type,
                    overlays,
                    ..
                } => {
                    let hitbox_kind = if overlays.is_empty() {
                        match image_type {
                            ImageType::TwitchGif { alt_text } => HitboxKind::Url {
                                url: url.clone(),
                                display_text: alt_text.clone(),
                                embed_id: 0,
                            },
                            ImageType::Emote { name, tooltip_meta } => HitboxKind::Emote {
                                name: name.clone(),
                                url: url.clone(),
                                meta: tooltip_meta.clone(),
                            },
                            ImageType::Badge {
                                name,
                                description,
                                tooltip_meta,
                                ..
                            } => HitboxKind::Badge {
                                name: name.clone(),
                                url: url.clone(),
                                description: description.clone(),
                                meta: tooltip_meta.clone(),
                            },
                        }
                    } else {
                        let mut emotes = vec![];
                        if let ImageType::Emote { name, tooltip_meta } = image_type {
                            emotes.push(LayeredTooltipEmote {
                                name: name.clone(),
                                url: url.clone(),
                                meta: tooltip_meta.clone(),
                            });
                        }
                        for overlay in overlays {
                            emotes.push(LayeredTooltipEmote {
                                name: overlay.name.clone(),
                                url: overlay.url.clone(),
                                meta: overlay.tooltip_meta.clone(),
                            });
                        }
                        HitboxKind::LayeredEmote { emotes }
                    };

                    self.hitboxes.push(HitboxRegion {
                        bounds: Bounds::new(point(px(*x), px(*y)), size(px(*width), px(*height))),
                        kind: hitbox_kind,
                    });
                }
                LayoutSegment::LayeredImage {
                    layers,
                    x,
                    y,
                    width,
                    height,
                    ..
                } => {
                    let emotes = layers
                        .iter()
                        .map(|layer| LayeredTooltipEmote {
                            name: layer.name.clone(),
                            url: layer.url.clone(),
                            meta: layer.tooltip_meta.clone(),
                        })
                        .collect();

                    self.hitboxes.push(HitboxRegion {
                        bounds: Bounds::new(point(px(*x), px(*y)), size(px(*width), px(*height))),
                        kind: HitboxKind::LayeredEmote { emotes },
                    });
                }
                _ => {}
            }
        }

        self.segments.extend(new_segments);

        self.current_y += actual_height;

        self.line_hitbox_start_idx = self.hitboxes.len();
        self.line_mention_highlight_start_idx = self.mention_highlights.len();
    }

    /// Wrap to next line
    /// Start a new line when `width` more would overflow the right boundary.
    /// An item on an empty line stays there even if it is wider than the line.
    #[inline]
    pub(super) fn wrap_unless_fits(&mut self, width: f32) {
        if self.current_x + width > self.right_boundary && self.current_x > MESSAGE_PADDING_LEFT {
            self.wrap_line();
        }
    }

    /// Place one badge image (wrapping first if it does not fit) and advance
    /// past it and the inter-badge gap.
    pub(super) fn place_badge(&mut self, url: &str, is_animated: bool, badge: ImageType) {
        let size = badge_size();
        self.wrap_unless_fits(size);
        self.add_image(url, size, size, is_animated, badge, None);
        self.advance(size + badge_gap());
    }

    /// Close the current line, if it has content, so a full-width block
    /// (embed card, upload preview) starts below it.
    pub(super) fn break_to_block(&mut self) {
        self.flush_text_segment();
        self.flush_layered_emote();
        if !self.line_buffer.is_empty() {
            self.commit_line();
            self.current_line += 1;
            self.line_y_positions.push(self.current_y);
            self.current_x = self.content_start_x;
            self.text_segment_start_x = self.content_start_x;
            self.last_emote_buffer_idx = None;
            self.line_offsets.push(self.source_offset);
        }
    }

    pub(super) fn wrap_line(&mut self) {
        self.flush_text_segment();
        self.flush_layered_emote();

        self.commit_line();

        self.current_line += 1;

        self.line_y_positions.push(self.current_y);

        self.last_emote_buffer_idx = None;
        self.last_media_height = None;

        let indent = if self.content_started {
            if self.show_timestamp {
                self.timestamp_column_end_x
            } else {
                self.content_start_x
            }
        } else {
            MESSAGE_PADDING_LEFT
        };

        let indent =
            indent.min((self.right_boundary - MIN_WRAPPED_CONTENT_WIDTH).max(MESSAGE_PADDING_LEFT));

        self.current_x = indent;
        self.text_segment_start_x = indent;

        self.line_offsets.push(self.source_offset);
    }

    /// Wrap at authored whitespace that does not fit the line. The whitespace
    /// is not shaped, but it stays in the source text so copied text and word
    /// selection keep the gap between the words on either side of the wrap.
    pub(super) fn wrap_at_whitespace(&mut self, whitespace: &str) {
        self.flush_text_segment();
        self.append_source_text_at_current_x(whitespace);
        self.wrap_line();
    }

    /// Commit an authored hard line break without shaping it as a whitespace
    /// glyph. Source and selection indices still retain the original newline
    /// bytes as Unicode scalar positions.
    pub(super) fn hard_break(&mut self, source_break: &str) {
        self.flush_text_segment();
        for ch in source_break.chars() {
            self.source_char_positions.push(self.current_x);
            self.source_text.push(ch);
            self.source_offset += 1;
        }
        self.last_text_ended_with_whitespace = true;
        self.last_token_type = LastTokenType::None;
        self.wrap_line();
    }

    /// Finalize and write results to layout
    pub(super) fn finalize(mut self, layout: &mut CanvasMessageLayout) {
        self.flush_text_segment();
        self.flush_layered_emote();

        self.commit_line();

        layout.height = self.current_y + MESSAGE_PADDING_VERTICAL;
        layout.line_count = self.current_line + 1;

        self.source_char_positions.push(self.current_x);
        debug_assert_eq!(
            self.source_char_positions.len(),
            self.source_text.chars().count() + 1,
            "canvas source positions must retain one boundary per Unicode scalar"
        );

        let mut final_segments = std::mem::take(&mut layout.segments);
        final_segments.extend(self.segments);
        layout.segments = final_segments;

        layout.shaped_lines = self.shaped_lines.into_vec();

        let mut final_hitboxes = std::mem::take(&mut layout.hitbox_regions);
        final_hitboxes.extend(self.hitboxes);
        layout.hitbox_regions = final_hitboxes;
        layout.mention_highlights = self.mention_highlights;

        layout.line_y_positions = self.line_y_positions;

        layout.source_text = self.source_text;
        layout.source_char_positions = self.source_char_positions;
        layout.pending_native_dimension_urls = self.pending_native_dimension_urls;
        layout.reserved_badge_channel = self.reserved_badge_channel;

        layout.animated_emote_count = self.animated_emote_count;
    }
}
