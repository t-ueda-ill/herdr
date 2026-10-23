use std::path::PathBuf;

use tracing::info;

use crate::layout::PaneId;

use super::terminal::GhosttyPaneCore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DefaultColorQuery {
    Foreground,
    Background,
    Cursor,
}

impl DefaultColorQuery {
    pub(super) fn osc_number(self) -> u8 {
        match self {
            Self::Foreground => 10,
            Self::Background => 11,
            Self::Cursor => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DefaultColorEvent {
    Query(DefaultColorQuery),
    Set(DefaultColorQuery),
    Reset(DefaultColorQuery),
    PaletteQuery(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DefaultColorTrackedEvent {
    pub(super) end_offset: usize,
    pub(super) event: DefaultColorEvent,
}

#[derive(Debug, Default)]
pub(super) struct DefaultColorOscTracker {
    state: DefaultColorOscTrackerState,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DefaultColorOscTrackerState {
    #[default]
    Ground,
    Escape,
    OscBody,
    OscEscape,
    IgnoreString,
    IgnoreStringEscape,
    OversizedOsc,
    OversizedOscEscape,
}

fn is_ignored_string_intro(byte: u8) -> bool {
    matches!(byte, b'P' | b'_' | b'^' | b'X')
}

impl DefaultColorOscTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        let mut saw_default_color_set = false;
        let mut cursor = 0;
        while cursor < bytes.len() {
            if matches!(
                self.state,
                DefaultColorOscTrackerState::Ground | DefaultColorOscTrackerState::IgnoreString
            ) {
                let Some(offset) = bytes[cursor..].iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                cursor += offset;
            }
            let byte = bytes[cursor];
            cursor += 1;
            match self.state {
                DefaultColorOscTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    }
                }
                DefaultColorOscTrackerState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::OscBody;
                    } else if is_ignored_string_intro(byte) {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    } else if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    } else {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OscBody => match byte {
                    0x07 => {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                    0x1b => self.state = DefaultColorOscTrackerState::OscEscape,
                    _ => self.body.push(byte),
                },
                DefaultColorOscTrackerState::OscEscape => {
                    if byte == b'\\' {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = DefaultColorOscTrackerState::OscBody;
                    }
                }
                DefaultColorOscTrackerState::IgnoreString => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreStringEscape;
                    }
                }
                DefaultColorOscTrackerState::IgnoreStringEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    }
                }
                DefaultColorOscTrackerState::OversizedOsc => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOscEscape;
                    } else if byte == 0x07 {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OversizedOscEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOsc;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::OversizedOsc;
            }
        }

        saw_default_color_set
    }
}

fn is_default_color_set_osc(body: &[u8]) -> bool {
    parse_default_color_events(body)
        .iter()
        .any(|event| matches!(event, DefaultColorEvent::Set(_)))
}

#[derive(Debug, Default)]
pub(super) struct DefaultColorEventTracker {
    state: DefaultColorOscTrackerState,
    body: Vec<u8>,
    pending: Vec<DefaultColorTrackedEvent>,
}

impl DefaultColorEventTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        let mut cursor = 0;
        while cursor < bytes.len() {
            if matches!(
                self.state,
                DefaultColorOscTrackerState::Ground | DefaultColorOscTrackerState::IgnoreString
            ) {
                let Some(offset) = bytes[cursor..].iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                cursor += offset;
            }
            let index = cursor;
            let byte = bytes[cursor];
            cursor += 1;
            match self.state {
                DefaultColorOscTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    }
                }
                DefaultColorOscTrackerState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::OscBody;
                    } else if is_ignored_string_intro(byte) {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    } else if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    } else {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OscBody => match byte {
                    0x07 => {
                        self.finalize(index + 1);
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                    0x1b => self.state = DefaultColorOscTrackerState::OscEscape,
                    _ => self.body.push(byte),
                },
                DefaultColorOscTrackerState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize(index + 1);
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = DefaultColorOscTrackerState::OscBody;
                    }
                }
                DefaultColorOscTrackerState::IgnoreString => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreStringEscape;
                    }
                }
                DefaultColorOscTrackerState::IgnoreStringEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    }
                }
                DefaultColorOscTrackerState::OversizedOsc => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOscEscape;
                    } else if byte == 0x07 {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OversizedOscEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOsc;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::OversizedOsc;
            }
        }
    }

    fn finalize(&mut self, end_offset: usize) {
        self.pending.extend(
            parse_default_color_events(&self.body)
                .into_iter()
                .map(|event| DefaultColorTrackedEvent { end_offset, event }),
        );
        self.body.clear();
    }

    pub(super) fn in_progress_event(&self) -> Option<DefaultColorEvent> {
        if !matches!(
            self.state,
            DefaultColorOscTrackerState::OscBody | DefaultColorOscTrackerState::OscEscape
        ) {
            return None;
        }
        let mut events = parse_default_color_events(&self.body);
        (events.len() == 1).then(|| events.remove(0))
    }

    pub(super) fn drain_pending(&mut self) -> Vec<DefaultColorTrackedEvent> {
        std::mem::take(&mut self.pending)
    }
}

fn parse_default_color_events(body: &[u8]) -> Vec<DefaultColorEvent> {
    let single = match body {
        b"10;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Foreground)),
        b"11;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Background)),
        b"12;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Cursor)),
        b"110" | b"110;" => Some(DefaultColorEvent::Reset(DefaultColorQuery::Foreground)),
        b"111" | b"111;" => Some(DefaultColorEvent::Reset(DefaultColorQuery::Background)),
        _ => parse_palette_color_query(body),
    };
    if let Some(event) = single {
        return vec![event];
    }
    parse_default_color_set_events(body)
}

fn parse_palette_color_query(body: &[u8]) -> Option<DefaultColorEvent> {
    let index = body.strip_prefix(b"4;")?.strip_suffix(b";?")?;
    if index.is_empty() || index.len() > 3 || !index.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut value: u16 = 0;
    for &digit in index {
        value = value * 10 + u16::from(digit - b'0');
    }
    u8::try_from(value)
        .ok()
        .map(DefaultColorEvent::PaletteQuery)
}

fn parse_default_color_set_events(body: &[u8]) -> Vec<DefaultColorEvent> {
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return Vec::new();
    };
    let start = match &body[..separator] {
        b"10" => 10,
        b"11" => 11,
        b"12" => 12,
        _ => return Vec::new(),
    };
    body[separator + 1..]
        .split(|byte| *byte == b';')
        .filter(|value| !value.is_empty())
        .enumerate()
        .filter_map(|(offset, value)| {
            if value == b"?" {
                return None;
            }
            let query = match start + offset {
                10 => DefaultColorQuery::Foreground,
                11 => DefaultColorQuery::Background,
                12 => DefaultColorQuery::Cursor,
                _ => return None,
            };
            Some(DefaultColorEvent::Set(query))
        })
        .collect()
}

pub(super) fn parse_reported_cwd(value: &[u8], shell_off_host: bool) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?.trim();
    if value.starts_with("file://") {
        return parse_file_uri_cwd(value, shell_off_host);
    }
    let path = value.trim_matches('"');
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Collects complete OSC bodies from a raw byte stream. Consumers receive only
/// bodies, keeping the framing state machine independent from OSC commands.
#[derive(Debug, Default)]
struct OscStreamCollector {
    state: OscStreamState,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OscStreamState {
    #[default]
    Ground,
    Escape,
    Body,
    BodyEscape,
    IgnoringString,
    IgnoringStringEscape,
    Discarding,
    DiscardingEscape,
}

impl OscStreamCollector {
    const MAX_BODY_BYTES: usize = 4096;

    fn observe(&mut self, bytes: &[u8], mut receive: impl FnMut(&[u8])) {
        let mut cursor = 0;
        while cursor < bytes.len() {
            if matches!(
                self.state,
                OscStreamState::Ground | OscStreamState::IgnoringString
            ) {
                let Some(offset) = bytes[cursor..].iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                cursor += offset;
            }
            let byte = bytes[cursor];
            cursor += 1;
            match self.state {
                OscStreamState::Ground => {
                    if byte == 0x1b {
                        self.state = OscStreamState::Escape;
                    }
                }
                OscStreamState::Escape => match byte {
                    b']' => {
                        self.body.clear();
                        self.state = OscStreamState::Body;
                    }
                    0x1b => self.state = OscStreamState::Escape,
                    byte if is_ignored_string_intro(byte) => {
                        self.state = OscStreamState::IgnoringString;
                    }
                    _ => self.state = OscStreamState::Ground,
                },
                OscStreamState::Body => match byte {
                    0x07 => self.finish(&mut receive),
                    0x1b => self.state = OscStreamState::BodyEscape,
                    _ => self.push(byte),
                },
                OscStreamState::BodyEscape => match byte {
                    b'\\' => self.finish(&mut receive),
                    0x07 => {
                        self.push(0x1b);
                        if matches!(self.state, OscStreamState::Body) {
                            self.finish(&mut receive);
                        } else {
                            self.state = OscStreamState::Ground;
                        }
                    }
                    0x1b => {
                        self.push(0x1b);
                        self.state = match self.state {
                            OscStreamState::Body => OscStreamState::BodyEscape,
                            OscStreamState::Discarding => OscStreamState::DiscardingEscape,
                            state => state,
                        };
                    }
                    _ => {
                        self.push(0x1b);
                        if matches!(self.state, OscStreamState::Body) {
                            self.push(byte);
                        }
                    }
                },
                OscStreamState::IgnoringString => {
                    if byte == 0x1b {
                        self.state = OscStreamState::IgnoringStringEscape;
                    }
                }
                OscStreamState::IgnoringStringEscape => {
                    if byte == b'\\' {
                        self.state = OscStreamState::Ground;
                    } else if byte != 0x1b {
                        self.state = OscStreamState::IgnoringString;
                    }
                }
                OscStreamState::Discarding => {
                    if byte == 0x07 {
                        self.state = OscStreamState::Ground;
                    } else if byte == 0x1b {
                        self.state = OscStreamState::DiscardingEscape;
                    }
                }
                OscStreamState::DiscardingEscape => {
                    if byte == b'\\' {
                        self.state = OscStreamState::Ground;
                    } else if byte != 0x1b {
                        self.state = OscStreamState::Discarding;
                    }
                }
            }
        }
    }

    fn push(&mut self, byte: u8) {
        self.body.push(byte);
        if self.body.len() > Self::MAX_BODY_BYTES {
            self.body.clear();
            self.state = OscStreamState::Discarding;
        } else {
            self.state = OscStreamState::Body;
        }
    }

    fn finish(&mut self, receive: &mut impl FnMut(&[u8])) {
        receive(&self.body);
        self.body.clear();
        self.state = OscStreamState::Ground;
    }
}

/// Maximum retained string length for agent OSC title and progress payloads.
/// Title text is untrusted model output; cap it to bound memory and log size.
const AGENT_OSC_MAX_CHARS: usize = 256;

/// Always-on tracker that retains the latest OSC 0/2 title and OSC 9 progress
/// payload emitted by the child process. Nothing here affects rendering; this
/// is pure passive capture for the detection engine (Stage C / Stage D).
///
/// - `latest_title` — last OSC 0 or OSC 2 payload, sanitized. An empty
///   payload (e.g. `\x1b]0;\x07`) clears the stored value.
/// - `latest_progress` — last OSC 9 payload (the part after `9;`), stored
///   as-is after sanitization. E.g. `"4;3;"` or `"4;0;"`.
#[derive(Debug, Default)]
pub(super) struct AgentOscStateTracker {
    collector: OscStreamCollector,
    latest_title: Option<String>,
    terminal_title: Option<String>,
    latest_progress: Option<String>,
    offhost_claim: OffHostClaimState,
}

/// Private OSC number carrying `herdr-agent;v=1;...` claims from a remote
/// shell function. 43737 is "herdr" on a phone keypad. Why not 21337: that
/// number is already recorded as debug evidence by `OscDebugTracker`, and an
/// authoritative contract must not share a number with passive evidence.
/// 43737 is outside every range that terminals and multiplexers define
/// (0-9, 99, 133, 633, 777, 1337, 3008, 5522, ...), so a collision would need
/// another program to pick the same arbitrary number.
pub(super) const OFFHOST_AGENT_OSC: &[u8] = b"43737";
const OFFHOST_CLAIM_NAMESPACE: &[u8] = b"herdr-agent";
const OFFHOST_CLAIM_VERSION: &[u8] = b"v=1";
const OFFHOST_CLAIM_FIELD_MAX_BYTES: usize = 64;

/// What a remote shell function said about an agent running in this pane.
/// It is a claim, not evidence: any byte stream reaching the pane can forge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OffHostAgentClaim {
    Started { label: String },
    Exited,
}

/// Claims live outside `latest_title` / `latest_progress` so that
/// `clear_retained` on an agent transition cannot erase a claim that caused
/// that very transition.
/// A claim newer than what the caller has handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OffHostClaimUpdate {
    pub(super) generation: u64,
    pub(super) claim: OffHostAgentClaim,
    /// Generation of the newest matched exit. It can precede `generation`
    /// when a start overwrote the exit before the caller polled, and the
    /// caller must still report that exit.
    pub(super) released_generation: u64,
}

/// Positions a caller moves its cursor to after a detector reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OffHostClaimMarks {
    /// Everything up to here counts as handled.
    pub(super) generation: u64,
    /// Handled up to here, except that a live `Started` claim is delivered again.
    pub(super) rewound_generation: u64,
    pub(super) released_generation: u64,
}

#[derive(Debug, Default)]
struct OffHostClaimState {
    generation: u64,
    released_generation: u64,
    latest: Option<OffHostAgentClaim>,
    /// Pairs an exit with its start. A stale exit from an earlier invocation
    /// must not release a newer claim, so it is dropped here.
    active_token: Option<String>,
}

impl OffHostClaimState {
    /// Returns true when a `start` was recorded.
    fn observe(&mut self, payload: &[u8]) -> bool {
        let Some(claim) = parse_offhost_claim(payload) else {
            return false;
        };
        match claim {
            ParsedOffHostClaim::Start { token, label } => {
                self.active_token = Some(token);
                self.latest = Some(OffHostAgentClaim::Started { label });
                self.generation += 1;
                true
            }
            ParsedOffHostClaim::Exit { token } => {
                if self.active_token.as_deref() != Some(token.as_str()) {
                    return false;
                }
                self.active_token = None;
                self.latest = Some(OffHostAgentClaim::Exited);
                self.generation += 1;
                self.released_generation = self.generation;
                false
            }
        }
    }
}

enum ParsedOffHostClaim {
    Start { token: String, label: String },
    Exit { token: String },
}

fn offhost_claim_field(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > OFFHOST_CLAIM_FIELD_MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.chars().any(char::is_control)).then(|| text.to_string())
}

fn parse_offhost_claim(payload: &[u8]) -> Option<ParsedOffHostClaim> {
    let mut fields = payload.split(|&byte| byte == b';');
    if fields.next()? != OFFHOST_CLAIM_NAMESPACE || fields.next()? != OFFHOST_CLAIM_VERSION {
        return None;
    }
    let verb = fields.next()?;
    let token = offhost_claim_field(fields.next()?)?;
    let claim = match verb {
        b"start" => ParsedOffHostClaim::Start {
            token,
            label: offhost_claim_field(fields.next()?)?,
        },
        b"exit" => ParsedOffHostClaim::Exit { token },
        _ => return None,
    };
    // Extra fields mean a newer writer; v=1 readers do not guess their meaning.
    fields.next().is_none().then_some(claim)
}

impl AgentOscStateTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        let (collector, latest_title, terminal_title, latest_progress, offhost_claim) = (
            &mut self.collector,
            &mut self.latest_title,
            &mut self.terminal_title,
            &mut self.latest_progress,
            &mut self.offhost_claim,
        );
        let mut terminal_title_changed = false;
        collector.observe(bytes, |body| {
            let Some((command, payload)) = parse_agent_osc_body(body) else {
                return;
            };
            match command {
                b"0" | b"2" => {
                    let title = sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS);
                    let title = (!title.is_empty()).then_some(title);
                    terminal_title_changed |= *terminal_title != title;
                    *terminal_title = title.clone();
                    *latest_title = title;
                }
                b"9" => {
                    *latest_progress =
                        Some(sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS));
                }
                OFFHOST_AGENT_OSC => {
                    // The shell's own title and progress predate the agent and
                    // must not become its evidence.
                    let started = offhost_claim.observe(payload);
                    if started {
                        *latest_title = None;
                        *latest_progress = None;
                    }
                }
                _ => {}
            }
        });
        terminal_title_changed
    }

    /// Returns the newest claim only when it is newer than `seen_generation`,
    /// so the per-tick poll allocates nothing while no claim arrives.
    pub(super) fn offhost_claim_since(&self, seen_generation: u64) -> Option<OffHostClaimUpdate> {
        let claim = &self.offhost_claim;
        if claim.generation <= seen_generation {
            return None;
        }
        claim.latest.clone().map(|latest| OffHostClaimUpdate {
            generation: claim.generation,
            claim: latest,
            released_generation: claim.released_generation,
        })
    }

    /// The start claim that has not exited yet, as `(kind, token)`.
    #[cfg(unix)]
    pub(super) fn offhost_active_claim(&self) -> Option<(String, String)> {
        let claim = &self.offhost_claim;
        match (&claim.latest, &claim.active_token) {
            (Some(OffHostAgentClaim::Started { label }), Some(token)) => {
                Some((label.clone(), token.clone()))
            }
            _ => None,
        }
    }

    /// Restores a claim carried across a handoff. It lands as the first
    /// claim, so the claim watcher adopts it on its first poll, and the token
    /// keeps the agent's later exit marker valid.
    #[cfg(unix)]
    pub(super) fn seed_offhost_claim(&mut self, label: String, token: String) {
        self.offhost_claim.active_token = Some(token);
        self.offhost_claim.latest = Some(OffHostAgentClaim::Started { label });
        self.offhost_claim.generation += 1;
    }

    #[cfg(test)]
    fn offhost_claim_generation(&self) -> u64 {
        self.offhost_claim.generation
    }

    pub(super) fn offhost_claim_marks(&self) -> OffHostClaimMarks {
        let claim = &self.offhost_claim;
        let live_start = matches!(claim.latest, Some(OffHostAgentClaim::Started { .. }));
        OffHostClaimMarks {
            generation: claim.generation,
            rewound_generation: claim.generation - u64::from(live_start),
            released_generation: claim.released_generation,
        }
    }

    pub(super) fn terminal_title(&self) -> Option<&str> {
        self.terminal_title.as_deref()
    }

    #[cfg(unix)]
    pub(super) fn seed_terminal_title(&mut self, title: Option<String>) {
        self.terminal_title = title;
    }

    /// Returns the latest retained OSC title, or `""` if none has been seen or
    /// the last title was an empty clear.
    #[allow(dead_code)] // used by terminal.rs; full call chain wired in Stage C
    pub(super) fn latest_title(&self) -> &str {
        self.latest_title.as_deref().unwrap_or("")
    }

    /// Returns the latest retained OSC 9 progress payload, or `""` if none.
    #[allow(dead_code)] // used by terminal.rs; full call chain wired in Stage C
    pub(super) fn latest_progress(&self) -> &str {
        self.latest_progress.as_deref().unwrap_or("")
    }

    /// Drops the retained title and progress so a new foreground agent cannot
    /// inherit OSC evidence emitted by a previous process. The in-flight parse
    /// state is kept: a sequence spanning the agent change finalizes normally
    /// and is attributed to the new agent.
    pub(super) fn clear_retained(&mut self) {
        self.latest_title = None;
        self.latest_progress = None;
    }
}

/// Splits an OSC body at the first `;`, returning `(command, payload)`.
/// Returns `None` if there is no `;`.
fn parse_agent_osc_body(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let sep = body.iter().position(|&b| b == b';')?;
    Some((&body[..sep], &body[sep + 1..]))
}

fn sanitize_agent_osc_string(payload: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(payload);
    let mut out = String::new();
    for ch in text.chars().filter(|ch| !ch.is_control()).take(max_chars) {
        out.push(ch);
    }
    out
}

/// Reconstructs selected OSC sequences for local evidence capture while
/// debugging agent title/status behavior. This is intentionally passive:
/// nothing here affects terminal rendering or detection state.
#[derive(Debug)]
pub(super) struct OscDebugTracker {
    enabled: bool,
    collector: OscStreamCollector,
    pending: Vec<OscDebugEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OscDebugEvent {
    pub(super) command: String,
    pub(super) payload: String,
}

impl OscDebugTracker {
    pub(super) fn from_env() -> Self {
        Self {
            enabled: osc_debug_enabled_from_env(),
            collector: OscStreamCollector::default(),
            pending: Vec::new(),
        }
    }

    pub(super) fn observe(&mut self, bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        let (collector, pending) = (&mut self.collector, &mut self.pending);
        collector.observe(bytes, |body| {
            if let Some(event) = parse_osc_debug_event(body) {
                pending.push(event);
            }
        });
    }

    pub(super) fn drain_pending(&mut self) -> Vec<OscDebugEvent> {
        std::mem::take(&mut self.pending)
    }
}

impl Default for OscDebugTracker {
    fn default() -> Self {
        Self::from_env()
    }
}

fn osc_debug_enabled_from_env() -> bool {
    std::env::var("HERDR_DEBUG_OSC_EVIDENCE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn parse_osc_debug_event(body: &[u8]) -> Option<OscDebugEvent> {
    let separator = body.iter().position(|byte| *byte == b';')?;
    let command = &body[..separator];
    let payload = &body[separator + 1..];
    if !matches!(command, b"0" | b"2" | b"9" | b"21337") {
        return None;
    }
    Some(OscDebugEvent {
        command: std::str::from_utf8(command).ok()?.to_string(),
        payload: sanitized_osc_debug_payload(payload),
    })
}

fn sanitized_osc_debug_payload(payload: &[u8]) -> String {
    const MAX_CHARS: usize = 512;
    let text = String::from_utf8_lossy(payload);
    let mut sanitized = String::new();
    for ch in text.chars().filter(|ch| !ch.is_control()).take(MAX_CHARS) {
        sanitized.push(ch);
    }
    if text.chars().count() > MAX_CHARS {
        sanitized.push_str("...");
    }
    sanitized
}

/// Why a host check at all: OSC 7 from a local shell naming another machine
/// means a nested `ssh` or `su` session whose paths do not exist here. Why it
/// is skipped off-host: the shell there names its own host, which is never ours.
fn parse_file_uri_cwd(uri: &str, shell_off_host: bool) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else if let Some(slash) = rest.find('/') {
        let host = &rest[..slash];
        if !(shell_off_host || host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        &rest[slash..]
    } else {
        rest
    };
    let path = percent_decode_utf8(path)?;

    #[cfg(windows)]
    {
        let mut path = path;
        if path.len() >= 3
            && path.as_bytes()[0] == b'/'
            && path.as_bytes()[2] == b':'
            && path.as_bytes()[1].is_ascii_alphabetic()
        {
            path.remove(0);
        }
        Some(PathBuf::from(path.replace('/', "\\")))
    }

    #[cfg(not(windows))]
    Some(PathBuf::from(path))
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            let hi = *bytes.get(idx + 1)?;
            let lo = *bytes.get(idx + 2)?;
            output.push(hex_value(hi)? * 16 + hex_value(lo)?);
            idx += 3;
        } else {
            output.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn foreground_job_is_shell(job: &crate::platform::ForegroundJob, shell_pid: u32) -> bool {
    job.processes.iter().any(|process| process.pid == shell_pid)
}

pub(super) fn current_transient_default_color_owner(shell_pid: u32) -> Option<u32> {
    let job = crate::detect::foreground_job(shell_pid)?;
    (!foreground_job_is_shell(&job, shell_pid)).then_some(job.process_group_id)
}

#[cfg(target_os = "macos")]
pub(super) fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    let _ = owner_pgid;
    foreground_job_is_shell(foreground_job, shell_pid)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    foreground_job.process_group_id != owner_pgid
        && foreground_job_is_shell(foreground_job, shell_pid)
}

pub(super) fn write_host_terminal_theme(
    terminal: &mut crate::ghostty::Terminal,
    theme: crate::terminal_theme::TerminalTheme,
) {
    write_host_terminal_theme_selective(terminal, theme, true, true);
}

pub(super) fn write_host_terminal_theme_selective(
    terminal: &mut crate::ghostty::Terminal,
    theme: crate::terminal_theme::TerminalTheme,
    foreground: bool,
    background: bool,
) {
    if foreground {
        write_host_default_color(
            terminal,
            crate::terminal_theme::DefaultColorKind::Foreground,
            theme.foreground,
        );
    }
    if background {
        write_host_default_color(
            terminal,
            crate::terminal_theme::DefaultColorKind::Background,
            theme.background,
        );
    }
}

fn write_host_default_color(
    terminal: &mut crate::ghostty::Terminal,
    kind: crate::terminal_theme::DefaultColorKind,
    color: Option<crate::terminal_theme::RgbColor>,
) {
    let sequence = if let Some(color) = color {
        crate::terminal_theme::osc_set_default_color_sequence(kind, color)
    } else {
        crate::terminal_theme::osc_reset_default_color_sequence(kind).to_string()
    };
    terminal.write(sequence.as_bytes());
}

pub(super) fn restore_host_terminal_theme_if_needed(
    core: &mut GhosttyPaneCore,
    pane_id: PaneId,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    let Some(owner_pgid) = core.transient_default_color_owner_pgid else {
        return false;
    };
    if core.host_terminal_theme.is_empty() {
        return false;
    }
    if !should_restore_host_terminal_theme(owner_pgid, shell_pid, alternate_screen, foreground_job)
    {
        return false;
    }

    core.transient_default_color_owner_pgid = None;
    core.child_default_foreground_changed = false;
    core.child_default_background_changed = false;
    write_host_terminal_theme(&mut core.terminal, core.host_terminal_theme);
    info!(
        pane = pane_id.raw(),
        owner_pgid, "restored host terminal default colors after transient override"
    );
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn bulk_osc_scans_match_bytewise_state_and_response_offsets() {
        use super::*;
        let mut input = b"text\x1b_Gm=1;".to_vec();
        input.extend(std::iter::repeat_n(b'A', 8192));
        input.extend_from_slice(b"\x1b\\\x1b]10;?\x07\x1b]11;red\x1b\\\x1bPignored");
        input.extend(0u8..=255);
        input.extend_from_slice(b"\x1b\\\x1b]12;");
        input.extend(std::iter::repeat_n(b'B', 4200));
        input.extend_from_slice(b"\x07\x1b]10;?\x1b\\\x1b]11;?\x07\x1b");
        for chunk_size in [1, 2, 3, 17, 4096, input.len()] {
            let mut bulk = DefaultColorOscTracker::default();
            let mut scalar = DefaultColorOscTracker::default();
            let mut bulk_events = DefaultColorEventTracker::default();
            let mut scalar_events = DefaultColorEventTracker::default();
            let mut bulk_stream = OscStreamCollector::default();
            let mut scalar_stream = OscStreamCollector::default();
            for chunk in input.chunks(chunk_size) {
                let changed = bulk.observe(chunk);
                let mut scalar_changed = false;
                let mut expected_events = Vec::new();
                let mut expected_bodies = Vec::new();
                for (offset, byte) in chunk.iter().enumerate() {
                    let byte = std::slice::from_ref(byte);
                    scalar_changed |= scalar.observe(byte);
                    scalar_events.observe(byte);
                    expected_events.extend(scalar_events.drain_pending().into_iter().map(
                        |mut event| {
                            event.end_offset += offset;
                            event
                        },
                    ));
                    scalar_stream.observe(byte, |body| expected_bodies.push(body.to_vec()));
                }
                bulk_events.observe(chunk);
                let mut bodies = Vec::new();
                bulk_stream.observe(chunk, |body| bodies.push(body.to_vec()));
                assert_eq!(changed, scalar_changed);
                assert_eq!((bulk.state, &bulk.body), (scalar.state, &scalar.body));
                assert_eq!(bulk_events.drain_pending(), expected_events);
                assert_eq!(
                    (bulk_events.state, &bulk_events.body),
                    (scalar_events.state, &scalar_events.body)
                );
                assert_eq!(bodies, expected_bodies);
                assert_eq!(
                    (bulk_stream.state, &bulk_stream.body),
                    (scalar_stream.state, &scalar_stream.body)
                );
            }
        }
    }

    use tokio::sync::mpsc;

    use super::*;
    use crate::layout::PaneId;

    fn pane_default_theme(
        pane: &super::super::GhosttyPaneTerminal,
    ) -> crate::terminal_theme::TerminalTheme {
        let mut core = pane.core.lock().unwrap();
        let super::super::terminal::GhosttyPaneCore {
            terminal,
            render_state,
            ..
        } = &mut *core;
        render_state.update(terminal).unwrap();
        let colors = render_state.colors().unwrap();
        crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: colors.foreground.r,
                g: colors.foreground.g,
                b: colors.foreground.b,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: colors.background.r,
                g: colors.background.g,
                b: colors.background.b,
            }),
            ..Default::default()
        }
    }

    fn shell_job(shell_pid: u32) -> crate::platform::ForegroundJob {
        crate::platform::ForegroundJob {
            process_group_id: shell_pid,
            processes: vec![crate::platform::ForegroundProcess {
                pid: shell_pid,
                name: "zsh".to_string(),
                argv0: Some("zsh".to_string()),
                argv: Some(vec!["zsh".to_string()]),
                cmdline: Some("zsh".to_string()),
            }],
        }
    }

    fn tracked_default_color_events(
        events: Vec<DefaultColorTrackedEvent>,
    ) -> Vec<DefaultColorEvent> {
        events.into_iter().map(|event| event.event).collect()
    }

    fn enabled_osc_debug_tracker() -> OscDebugTracker {
        OscDebugTracker {
            enabled: true,
            collector: OscStreamCollector::default(),
            pending: Vec::new(),
        }
    }

    #[test]
    fn osc_stream_collector_ignores_strings_and_preserves_escaped_bytes() {
        let mut collector = OscStreamCollector::default();
        let mut bodies = Vec::new();

        collector.observe(
            b"\x1bPignored\x1b]0;not-osc\x07\x1b\\\x1b]9;a\x1b",
            |body| bodies.push(body.to_vec()),
        );
        collector.observe(b"\x1b\\\x1b]2;b\x1b\x07", |body| bodies.push(body.to_vec()));

        assert_eq!(bodies, vec![b"9;a\x1b".to_vec(), b"2;b\x1b".to_vec()]);
    }

    #[test]
    fn default_color_tracker_detects_split_osc_11_sequences() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]11;rgb:11/22"));
        assert!(tracker.observe(b"/33\x1b\\"));
    }

    #[test]
    fn default_color_tracker_ignores_osc_queries() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]10;?\x1b\\"));
        assert!(!tracker.observe(b"\x1b]11;?\x07"));
    }

    #[test]
    fn reported_cwd_parses_file_uri_and_bare_paths() {
        assert_eq!(
            parse_reported_cwd(b"file:///tmp/herdr%20repo", false),
            Some(std::path::PathBuf::from("/tmp/herdr repo"))
        );
        assert_eq!(
            parse_reported_cwd(b"C:\\Users\\herdr\\src\\herdr", false),
            Some(std::path::PathBuf::from("C:\\Users\\herdr\\src\\herdr"))
        );
        assert_eq!(
            parse_reported_cwd(b"\"C:\\my proj\"", false),
            Some(std::path::PathBuf::from("C:\\my proj"))
        );
    }

    #[test]
    fn reported_cwd_rejects_invalid_or_empty_values() {
        assert_eq!(parse_reported_cwd(b"", false), None);
        assert_eq!(parse_reported_cwd(b"\xff", false), None);
        assert_eq!(parse_reported_cwd(b"file://remote/tmp", false), None);
    }

    #[test]
    fn reported_cwd_accepts_any_host_name_when_the_shell_runs_off_host() {
        // The other side's own host name is what its shell puts in OSC 7.
        assert_eq!(
            parse_reported_cwd(b"file://devbox.internal/home/u/my%20proj", true),
            Some(std::path::PathBuf::from("/home/u/my proj"))
        );
        assert_eq!(
            parse_reported_cwd(b"file://a1b2c3d4e5f6/work", true),
            Some(std::path::PathBuf::from("/work"))
        );
    }

    #[test]
    fn reported_cwd_keeps_the_local_forms_valid_off_host() {
        for off_host in [false, true] {
            assert_eq!(
                parse_reported_cwd(b"file:///tmp/x", off_host),
                Some(std::path::PathBuf::from("/tmp/x"))
            );
            assert_eq!(
                parse_reported_cwd(b"file://localhost/tmp/x", off_host),
                Some(std::path::PathBuf::from("/tmp/x"))
            );
        }
    }

    // -----------------------------------------------------------------------
    // AgentOscStateTracker tests
    // -----------------------------------------------------------------------

    #[test]
    fn agent_osc_osc0_title_with_bel() {
        let mut t = AgentOscStateTracker::default();
        t.observe("hello\x1b]0;braille title\x07world".as_bytes());
        assert_eq!(t.latest_title(), "braille title");
        assert_eq!(t.terminal_title(), Some("braille title"));
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_osc2_title_with_st() {
        let mut t = AgentOscStateTracker::default();
        t.observe("hello\x1b]2;static title\x1b\\world".as_bytes());
        assert_eq!(t.latest_title(), "static title");
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_empty_osc0_clears_title() {
        let mut t = AgentOscStateTracker::default();
        // First set a title.
        t.observe(b"\x1b]0;some title\x07");
        assert_eq!(t.latest_title(), "some title");
        // Then clear it with an empty payload (Codex pattern).
        t.observe(b"\x1b]0;\x07");
        assert_eq!(t.latest_title(), "");
        assert_eq!(t.terminal_title(), None);
    }

    #[test]
    fn clearing_agent_evidence_preserves_the_terminal_title() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe("\x1b]2;✳ 修复🙂标题\x1b\\".as_bytes());

        tracker.clear_retained();

        assert_eq!(tracker.latest_title(), "");
        assert_eq!(tracker.terminal_title(), Some("✳ 修复🙂标题"));
    }

    #[cfg(unix)]
    #[test]
    fn handoff_seed_does_not_restore_agent_detection_evidence() {
        let mut tracker = AgentOscStateTracker::default();

        tracker.seed_terminal_title(Some("✳ restored title".into()));

        assert_eq!(tracker.terminal_title(), Some("✳ restored title"));
        assert_eq!(tracker.latest_title(), "");
    }

    #[test]
    fn agent_osc_osc9_sets_progress_with_bel() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3;\x07");
        assert_eq!(t.latest_progress(), "4;3;");
        assert_eq!(t.latest_title(), "");
    }

    #[test]
    fn agent_osc_osc9_clear_progress_with_st() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3;\x07");
        assert_eq!(t.latest_progress(), "4;3;");
        t.observe(b"\x1b]9;4;0;\x1b\\");
        assert_eq!(t.latest_progress(), "4;0;");
    }

    #[test]
    fn agent_osc_split_sequence_across_chunks() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3");
        assert_eq!(t.latest_progress(), "");
        t.observe(b";\x07");
        assert_eq!(t.latest_progress(), "4;3;");
    }

    #[test]
    fn agent_osc_bel_and_st_terminators_both_work() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;title-bel\x07");
        assert_eq!(t.latest_title(), "title-bel");
        t.observe(b"\x1b]0;title-st\x1b\\");
        assert_eq!(t.latest_title(), "title-st");
    }

    #[test]
    fn agent_osc_oversized_payload_is_discarded_and_recovers() {
        let mut t = AgentOscStateTracker::default();
        // Set a title first.
        t.observe(b"\x1b]0;before\x07");
        assert_eq!(t.latest_title(), "before");

        // Feed an oversized OSC body (> 4096 bytes).
        let mut oversized = Vec::from(b"\x1b]0;".as_slice());
        oversized.extend(std::iter::repeat_n(b'x', 4097));
        oversized.push(0x07);
        t.observe(&oversized);
        // The oversized body is dropped; the previously stored title is kept.
        assert_eq!(t.latest_title(), "before");

        // After recovery, subsequent valid sequences are captured normally.
        t.observe(b"\x1b]0;after\x07");
        assert_eq!(t.latest_title(), "after");
    }

    fn claim_start(token: &str, kind: &str) -> Vec<u8> {
        format!("\x1b]43737;herdr-agent;v=1;start;{token};{kind}\x07").into_bytes()
    }

    fn claim_exit(token: &str) -> Vec<u8> {
        format!("\x1b]43737;herdr-agent;v=1;exit;{token}\x07").into_bytes()
    }

    #[test]
    fn offhost_claim_start_is_reported_once_per_generation() {
        let mut t = AgentOscStateTracker::default();
        t.observe(&claim_start("t1", "claude"));

        assert_eq!(
            t.offhost_claim_since(0),
            Some(OffHostClaimUpdate {
                generation: 1,
                claim: OffHostAgentClaim::Started {
                    label: "claude".into()
                },
                released_generation: 0,
            })
        );
        assert_eq!(t.offhost_claim_since(1), None);
    }

    #[test]
    fn offhost_claim_split_across_chunks_is_reassembled() {
        let mut t = AgentOscStateTracker::default();
        let bytes = claim_start("t1", "codex");
        for chunk in bytes.chunks(3) {
            t.observe(chunk);
        }

        assert!(matches!(
            t.offhost_claim_since(0),
            Some(OffHostClaimUpdate {
                generation: 1,
                claim: OffHostAgentClaim::Started { .. },
                ..
            })
        ));
    }

    #[test]
    fn offhost_claim_accepts_bel_and_st_terminators() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]43737;herdr-agent;v=1;start;t1;claude\x1b\\");
        assert_eq!(t.offhost_claim_generation(), 1);
        t.observe(b"\x1b]43737;herdr-agent;v=1;exit;t1\x07");
        assert_eq!(t.offhost_claim_generation(), 2);
        assert_eq!(
            t.offhost_claim_since(1),
            Some(OffHostClaimUpdate {
                generation: 2,
                claim: OffHostAgentClaim::Exited,
                released_generation: 2,
            })
        );
    }

    #[test]
    fn offhost_claim_over_the_body_cap_is_ignored() {
        let mut t = AgentOscStateTracker::default();
        let mut oversized = b"\x1b]43737;herdr-agent;v=1;start;t1;".to_vec();
        oversized.extend(std::iter::repeat_n(b'x', 4097));
        oversized.push(0x07);
        t.observe(&oversized);

        assert_eq!(t.offhost_claim_generation(), 0);
    }

    #[test]
    fn legacy_debug_osc_is_not_a_claim() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]21337;status=working\x07");
        t.observe(b"\x1b]21337;herdr-agent;v=1;start;t1;claude\x07");

        assert_eq!(t.offhost_claim_generation(), 0);
    }

    #[test]
    fn offhost_claim_with_unusable_fields_is_ignored() {
        let mut t = AgentOscStateTracker::default();
        for body in [
            "herdr-agent;v=2;start;t1;claude",
            "herdr-agent;v=1;start;t1",
            "herdr-agent;v=1;start;;claude",
            "herdr-agent;v=1;start;t1;claude;extra",
            "herdr-agent;v=1;pause;t1",
            "other;v=1;start;t1;claude",
        ] {
            t.observe(format!("\x1b]43737;{body}\x07").as_bytes());
        }

        assert_eq!(t.offhost_claim_generation(), 0);
    }

    #[test]
    fn offhost_claim_exit_with_a_foreign_token_is_ignored() {
        let mut t = AgentOscStateTracker::default();
        t.observe(&claim_exit("never-started"));
        assert_eq!(t.offhost_claim_generation(), 0);

        t.observe(&claim_start("new", "claude"));
        t.observe(&claim_exit("old"));
        assert_eq!(t.offhost_claim_generation(), 1);

        t.observe(&claim_exit("new"));
        assert_eq!(t.offhost_claim_generation(), 2);
    }

    #[test]
    fn offhost_claim_update_remembers_an_exit_that_a_later_start_overwrote() {
        let mut t = AgentOscStateTracker::default();
        t.observe(&claim_start("t1", "claude"));
        t.observe(&claim_exit("t1"));
        t.observe(&claim_start("t2", "claude"));

        let update = t.offhost_claim_since(1).expect("newer claim");

        assert_eq!(update.generation, 3);
        assert_eq!(update.released_generation, 2);
    }

    #[test]
    fn offhost_claim_marks_rewind_only_a_live_start() {
        let mut t = AgentOscStateTracker::default();
        t.observe(&claim_start("t1", "claude"));
        let live = t.offhost_claim_marks();
        assert_eq!((live.generation, live.rewound_generation), (1, 0));

        t.observe(&claim_exit("t1"));
        let exited = t.offhost_claim_marks();
        assert_eq!((exited.generation, exited.rewound_generation), (2, 2));
    }

    #[test]
    fn offhost_claim_survives_clear_retained() {
        let mut t = AgentOscStateTracker::default();
        t.observe(&claim_start("t1", "claude"));
        t.clear_retained();

        assert!(t.offhost_claim_since(0).is_some());
    }

    #[test]
    fn offhost_claim_start_drops_the_shells_retained_title_and_progress() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;user@host: ~\x07\x1b]9;4;3;\x07");
        t.observe(&claim_start("t1", "claude"));

        assert_eq!(t.latest_title(), "");
        assert_eq!(t.latest_progress(), "");
        assert_eq!(t.terminal_title(), Some("user@host: ~"));
    }

    #[test]
    fn offhost_claim_inside_a_dcs_string_is_ignored() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1bPtmux;\x1b\x1b]43737;herdr-agent;v=1;start;t1;claude\x07\x1b\\");

        assert_eq!(t.offhost_claim_generation(), 0);
    }

    #[test]
    fn agent_osc_cap_length_is_respected() {
        let mut t = AgentOscStateTracker::default();
        // Build a title of AGENT_OSC_MAX_CHARS + 50 ASCII chars.
        let long_title: String = "a".repeat(AGENT_OSC_MAX_CHARS + 50);
        let seq = format!("\x1b]0;{long_title}\x07");
        t.observe(seq.as_bytes());
        assert_eq!(t.latest_title().len(), AGENT_OSC_MAX_CHARS);
    }

    #[test]
    fn agent_osc_control_chars_stripped() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;before\x01after\x07");
        assert_eq!(t.latest_title(), "beforeafter");
    }

    #[test]
    fn agent_osc_unrelated_osc_does_not_overwrite_title() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;my title\x07");
        // OSC 4 (palette color), OSC 52 (clipboard) — should not touch title/progress.
        t.observe(b"\x1b]4;1;rgb:aa/bb/cc\x07");
        t.observe(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(t.latest_title(), "my title");
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_interleaved_sequences() {
        let mut t = AgentOscStateTracker::default();
        // OSC 0 title, then OSC 9 progress, then OSC 2 title update.
        t.observe(b"\x1b]0;first\x07\x1b]9;4;3;\x07\x1b]2;second\x07");
        assert_eq!(t.latest_title(), "second");
        assert_eq!(t.latest_progress(), "4;3;");
    }

    #[test]
    fn agent_osc_default_state_is_empty() {
        let t = AgentOscStateTracker::default();
        assert_eq!(t.latest_title(), "");
        assert_eq!(t.latest_progress(), "");
    }

    // -----------------------------------------------------------------------
    // OscDebugTracker tests (existing)
    // -----------------------------------------------------------------------

    #[test]
    fn osc_debug_tracker_detects_title_with_bel() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe("hello\x1b]0;✻ working title\x07world".as_bytes());

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "✻ working title".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_detects_title_with_st() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe("hello\x1b]2;static title\x1b\\world".as_bytes());

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "2".to_string(),
                payload: "static title".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_detects_split_status_sequences() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]9;4;3");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\x07\x1b]21337;status=working\x1b\\");

        assert_eq!(
            tracker.drain_pending(),
            vec![
                OscDebugEvent {
                    command: "9".to_string(),
                    payload: "4;3".to_string(),
                },
                OscDebugEvent {
                    command: "21337".to_string(),
                    payload: "status=working".to_string(),
                },
            ]
        );
    }

    #[test]
    fn osc_debug_tracker_ignores_untracked_osc_commands() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]52;c;SGVsbG8=\x07\x1b]7;file:///tmp\x07");

        assert!(tracker.drain_pending().is_empty());
    }

    #[test]
    fn osc_debug_tracker_sanitizes_control_characters() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]0;before\x01after\x07");

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "beforeafter".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_recovers_after_oversized_payload() {
        let mut tracker = enabled_osc_debug_tracker();
        let oversized = vec![b'a'; 4097];

        tracker.observe(b"\x1b]0;");
        tracker.observe(&oversized);
        tracker.observe(b"\x07\x1b]0;ok\x07");

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "ok".to_string(),
            }]
        );
    }

    #[test]
    fn default_color_event_tracker_detects_queries_sets_and_resets() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(
            b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07\x1b]4;0;?\x07\x1b]10;rgb:11/22/33\x07\x1b]111\x07",
        );

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![
                DefaultColorEvent::Query(DefaultColorQuery::Foreground),
                DefaultColorEvent::Query(DefaultColorQuery::Background),
                DefaultColorEvent::Query(DefaultColorQuery::Cursor),
                DefaultColorEvent::PaletteQuery(0),
                DefaultColorEvent::Set(DefaultColorQuery::Foreground),
                DefaultColorEvent::Reset(DefaultColorQuery::Background),
            ]
        );
    }

    #[test]
    fn default_color_event_tracker_tracks_each_multi_value_set() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(
            b"\x1b]10;rgb:11/22/33;rgb:44/55/66\x1b\\\x1b]10;?;rgb:77/88/99\x1b\\\x1b]10;;rgb:aa/bb/cc\x1b\\",
        );

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![
                DefaultColorEvent::Set(DefaultColorQuery::Foreground),
                DefaultColorEvent::Set(DefaultColorQuery::Background),
                DefaultColorEvent::Set(DefaultColorQuery::Background),
                DefaultColorEvent::Set(DefaultColorQuery::Foreground),
            ]
        );
    }

    #[test]
    fn default_color_event_tracker_handles_split_default_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]11");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b";?\x1b");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\\");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::Query(DefaultColorQuery::Background)]
        );
    }

    #[test]
    fn default_color_event_tracker_handles_split_palette_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]4;25");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"5;?\x1b");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\\");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::PaletteQuery(255)]
        );
    }

    #[test]
    fn default_color_event_tracker_rejects_malformed_palette_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]4;;?\x07");
        tracker.observe(b"\x1b]4;-1;?\x07");
        tracker.observe(b"\x1b]4;256;?\x07");
        tracker.observe(b"\x1b]4;0;?;1;?\x07");
        tracker.observe(b"\x1b]4;0;rgb:1111/2222/3333\x07");
        tracker.observe(b"\x1b]4;0;?\x07");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::PaletteQuery(0)]
        );
    }

    #[test]
    fn default_color_event_tracker_ignores_other_osc_and_dcs_payloads() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]0;title\x07");
        tracker.observe(b"\x1b]52;c;?\x07");
        tracker.observe(b"\x1bPtmux;\x1b\x1b]11;?\x07\x1b\\");
        tracker.observe(b"\x1bPtmux;payload\x07\x1b]11;?\x07\x1b\\");

        assert!(tracker.drain_pending().is_empty());
    }

    #[test]
    fn default_color_event_tracker_ignores_oversized_osc_until_terminator() {
        let mut tracker = DefaultColorEventTracker::default();
        let mut oversized = Vec::from(b"\x1b]11;".as_slice());
        oversized.extend(std::iter::repeat_n(b'a', 1025));
        oversized.extend_from_slice(b"\x1b]11;?\x07");

        tracker.observe(&oversized);
        assert!(tracker.drain_pending().is_empty());

        tracker.observe(b"\x1b]11;?\x07");
        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::Query(DefaultColorQuery::Background)]
        );
    }

    #[test]
    fn host_theme_restore_waits_for_shell_and_non_alternate_screen() {
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            true,
            Some(&shell_job(7)),
        ));
        assert!(!should_restore_host_terminal_theme(42, 7, false, None));
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&crate::platform::ForegroundJob {
                process_group_id: 42,
                processes: vec![crate::platform::ForegroundProcess {
                    pid: 42,
                    name: "droid".to_string(),
                    argv0: Some("droid".to_string()),
                    argv: Some(vec!["droid".to_string()]),
                    cmdline: Some("droid".to_string()),
                }],
            }),
        ));
        assert!(should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(target_os = "macos")]
        assert!(should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(not(target_os = "macos"))]
        assert!(!should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));
    }

    #[test]
    fn restore_host_terminal_theme_reapplies_cached_colors() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = super::super::GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let pane_id = PaneId::from_raw(1);
        let shell_pid = 7;
        let host_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x11,
                g: 0x22,
                b: 0x33,
            }),
            ..Default::default()
        };

        pane.apply_host_terminal_theme(host_theme);
        {
            let mut core = pane.core.lock().unwrap();
            core.transient_default_color_owner_pgid = Some(42);
            core.terminal.write(b"\x1b]11;rgb:dd/ee/ff\x1b\\");
        }
        assert_eq!(
            pane_default_theme(&pane).background,
            Some(crate::terminal_theme::RgbColor {
                r: 0xdd,
                g: 0xee,
                b: 0xff,
            })
        );

        {
            let mut core = pane.core.lock().unwrap();
            assert!(restore_host_terminal_theme_if_needed(
                &mut core,
                pane_id,
                shell_pid,
                false,
                Some(&shell_job(shell_pid)),
            ));
        }

        assert_eq!(pane_default_theme(&pane).background, host_theme.background);
        assert_eq!(pane_default_theme(&pane).foreground, host_theme.foreground);
    }
}
