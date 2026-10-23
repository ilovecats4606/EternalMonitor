//! Feedback by email: the message the window's Send feedback panel builds,
//! and when to ask for it once. Nothing here sends anything; the user's own
//! email app does, or they paste the copied text.
//!
//! The details never name the PC or its user and never carry an address.

use std::time::Duration;

pub const EMAIL: &str = "hello@sideband.studio";

/// Mail apps cut longer `mailto:` links, so the whole link stays under this.
pub const MAILTO_LIMIT: usize = 1800;

/// Said at the end of an email whose message had to be shortened.
pub const SHORTENED: &str = "[Message shortened. The full text is on your clipboard.]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Problem,
    Idea,
    Praise,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Problem => "Problem",
            Kind::Idea => "Idea",
            Kind::Praise => "Something I like",
        }
    }
}

/// What "Include app and PC details" adds, in compact lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Details {
    pub app_version: String,
    /// For example "Windows 11 24H2 (build 26100.2894)".
    pub windows: Option<String>,
    pub graphics: Vec<String>,
    pub second_screen: bool,
    pub ipad: Option<Ipad>,
}

/// The connected iPad, described by its screen. Its name is left out: it
/// is often the owner's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipad {
    pub usb: bool,
    pub screen_px: (u16, u16),
    pub refresh_hz: u8,
}

impl Details {
    fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "App: EternalMonitor {} for Windows",
            self.app_version
        )];
        if let Some(windows) = &self.windows {
            lines.push(format!("PC: {windows}"));
        }
        if !self.graphics.is_empty() {
            lines.push(format!("Graphics: {}", self.graphics.join(", ")));
        }
        let mode = if self.second_screen {
            "Second screen"
        } else {
            "Mirror"
        };
        lines.push(match self.ipad {
            Some(ipad) => format!(
                "Setup: {mode}, {}, iPad {} × {} at {} Hz",
                if ipad.usb { "USB" } else { "Wi-Fi" },
                ipad.screen_px.0,
                ipad.screen_px.1,
                ipad.refresh_hz
            ),
            None => format!("Setup: {mode}, no iPad connected"),
        });
        lines
    }

    /// One line for the panel, so people see what they would send.
    pub fn summary(&self) -> String {
        self.lines()
            .iter()
            .map(|line| line.split_once(": ").map_or(line.as_str(), |(_, v)| v))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// "EternalMonitor 0.3.2 for Windows: Problem", always with the kind and
/// version, so the studio can sort its mail. The version is no more than the
/// email's own "Sent from" line would say.
pub fn subject(kind: Kind) -> String {
    format!(
        "EternalMonitor {} for Windows: {}",
        env!("CARGO_PKG_VERSION"),
        kind.label()
    )
}

/// The email text: the message, then the details if included.
pub fn body(kind: Kind, message: &str, details: Option<&Details>) -> String {
    let message = message.trim();
    let mut text = String::from(message);
    let mut footer = vec![format!("Kind: {}", kind.label())];
    if let Some(details) = details {
        footer.extend(details.lines());
    }
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str("--\n");
    text.push_str(&footer.join("\n"));
    text
}

/// Subject and body for Copy message.
pub fn full_text(kind: Kind, message: &str, details: Option<&Details>) -> String {
    format!(
        "To: {EMAIL}\nSubject: {}\n\n{}",
        subject(kind),
        body(kind, message, details)
    )
}

/// Copy with logs: the message, then the app's recent log.
pub fn with_logs(message: &str, logs: &str) -> String {
    format!("{message}\n\n-- EternalMonitor log --\n{}", logs.trim_end())
}

/// A `mailto:` link no longer than [`MAILTO_LIMIT`]. A long message is cut
/// at a character boundary and the second value is true; the details always
/// fit, since they are a few short lines.
pub fn mailto(kind: Kind, message: &str, details: Option<&Details>) -> (String, bool) {
    let link = |body: &str| {
        format!(
            "mailto:{EMAIL}?subject={}&body={}",
            encode(&subject(kind)),
            encode(body)
        )
    };
    let whole = link(&body(kind, message, details));
    if whole.len() <= MAILTO_LIMIT {
        return (whole, false);
    }
    let message = message.trim();
    let marked = |kept: &str| format!("{}\n{SHORTENED}", kept.trim_end());
    // Measured around one kept character, since body() trims a message
    // that is only the note.
    let fixed = link(&body(kind, &marked("x"), details)).len() - 1;
    let mut budget = MAILTO_LIMIT.saturating_sub(fixed);
    let mut end = 0;
    for (i, c) in message.char_indices() {
        let cost = encoded_len(c);
        if cost > budget {
            break;
        }
        budget -= cost;
        end = i + c.len_utf8();
    }
    (link(&body(kind, &marked(&message[..end]), details)), true)
}

/// Percent-encodes everything but RFC 3986's unreserved characters. Line
/// breaks become CRLF, as RFC 6068 asks for in a body.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for c in text.chars() {
        push_encoded(&mut out, c);
    }
    out
}

fn push_encoded(out: &mut String, c: char) {
    match c {
        'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
        '\n' => out.push_str("%0D%0A"),
        '\r' => {}
        _ => {
            let mut bytes = [0; 4];
            for byte in c.encode_utf8(&mut bytes).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
}

fn encoded_len(c: char) -> usize {
    let mut out = String::new();
    push_encoded(&mut out, c);
    out.len()
}

/// "Windows 11 24H2 (build 26100.2894)" from the registry's DisplayVersion,
/// CurrentBuildNumber and UBR. Windows 11 still reports "Windows 10" as its
/// product name, so the build number decides.
pub fn windows_label(display_version: &str, build: &str, ubr: &str) -> Option<String> {
    let number: u32 = build.trim().parse().ok()?;
    let name = if number >= 22000 {
        "Windows 11"
    } else {
        "Windows 10"
    };
    let release = display_version.trim();
    let release = if release.is_empty() || release.starts_with("unavailable") {
        String::new()
    } else {
        format!(" {release}")
    };
    let build = match ubr.trim().parse::<u32>() {
        Ok(ubr) => format!("{number}.{ubr}"),
        Err(_) => number.to_string(),
    };
    Some(format!("{name}{release} (build {build})"))
}

// ── The one-time ask ────────────────────────────────────────────────────────

/// A session this long counts toward the ask.
pub const LONG_SESSION: Duration = Duration::from_secs(5 * 60);
/// The ask shows after this many long sessions.
pub const ASK_AFTER: u32 = 3;

/// Counts long streaming sessions and decides when to ask once. The count
/// and `done` persist in the settings file; `done` never goes back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ask {
    pub long_sessions: u32,
    pub done: bool,
    counted: Option<u32>,
}

impl Ask {
    pub fn new(long_sessions: u32, done: bool) -> Self {
        Self {
            long_sessions,
            done,
            counted: None,
        }
    }

    /// Called with the connected session's id and age, if one is connected.
    /// A session counts once, when it reaches [`LONG_SESSION`]. Returns true
    /// when the count changed and should be saved.
    pub fn observe(&mut self, session: Option<(u32, Duration)>) -> bool {
        let Some((id, age)) = session else {
            return false;
        };
        if self.done || age < LONG_SESSION || self.counted == Some(id) {
            return false;
        }
        self.counted = Some(id);
        if self.long_sessions >= ASK_AFTER {
            return false;
        }
        self.long_sessions += 1;
        true
    }

    /// Whether to show the card now. Never while an iPad is connected, so it
    /// is never on screen during a stream.
    pub fn due(&self, ipad_connected: bool) -> bool {
        !self.done && !ipad_connected && self.long_sessions >= ASK_AFTER
    }

    /// Send feedback or Not now: never ask again.
    pub fn finish(&mut self) {
        self.done = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details() -> Details {
        Details {
            app_version: "0.3.2".into(),
            windows: Some("Windows 11 24H2 (build 26100.2894)".into()),
            graphics: vec![
                "NVIDIA GeForce RTX 5080".into(),
                "AMD Radeon(TM) Graphics".into(),
            ],
            second_screen: true,
            ipad: Some(Ipad {
                usb: true,
                screen_px: (2420, 1668),
                refresh_hz: 120,
            }),
        }
    }

    fn decode(text: &str) -> String {
        let mut bytes = Vec::new();
        let raw = text.as_bytes();
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' {
                bytes.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                bytes.push(raw[i]);
                i += 1;
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    fn query(url: &str, key: &str) -> String {
        let query = url.split_once('?').unwrap().1;
        let value = query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .unwrap();
        decode(value)
    }

    #[test]
    fn details_are_compact_lines_with_no_names_or_addresses() {
        let body = body(Kind::Problem, "  The picture froze.  ", Some(&details()));
        assert_eq!(
            body,
            "The picture froze.\n\n--\nKind: Problem\nApp: EternalMonitor 0.3.2 for Windows\n\
             PC: Windows 11 24H2 (build 26100.2894)\n\
             Graphics: NVIDIA GeForce RTX 5080, AMD Radeon(TM) Graphics\n\
             Setup: Second screen, USB, iPad 2420 × 1668 at 120 Hz"
        );
        let mirror = Details {
            second_screen: false,
            ipad: None,
            ..details()
        };
        assert!(body_text(&mirror).contains("Setup: Mirror, no iPad connected"));
        let wifi = Details {
            ipad: Some(Ipad {
                usb: false,
                ..details().ipad.unwrap()
            }),
            ..details()
        };
        assert!(body_text(&wifi).contains("Second screen, Wi-Fi, iPad"));
        assert_eq!(
            details().summary(),
            "EternalMonitor 0.3.2 for Windows · Windows 11 24H2 (build 26100.2894) · \
             NVIDIA GeForce RTX 5080, AMD Radeon(TM) Graphics · Second screen, USB, iPad 2420 × 1668 at 120 Hz"
        );
    }

    fn body_text(details: &Details) -> String {
        body(Kind::Idea, "Hi", Some(details))
    }

    #[test]
    fn leaving_details_out_drops_versions_and_hardware() {
        let body = body(Kind::Idea, "Add a dark mode", None);
        assert_eq!(body, "Add a dark mode\n\n--\nKind: Idea");
        // The subject always has the kind and version, for sorting mail.
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            subject(Kind::Praise),
            format!("EternalMonitor {version} for Windows: Something I like")
        );
        let (url, _) = mailto(Kind::Idea, "Add a dark mode", None);
        let body = query(&url, "body");
        for hidden in [version, "Windows 11", "NVIDIA", "iPad", "Setup", "App:"] {
            assert!(!body.contains(hidden), "{hidden}");
        }
        // An empty message still says what kind it is.
        assert_eq!(
            super::body(Kind::Praise, "  ", None),
            "--\nKind: Something I like"
        );
    }

    #[test]
    fn mailto_encodes_every_reserved_character_and_line_break() {
        let message = "Lag & stutter? 50% of the time = bad #1\nÉcran: 画面";
        let (url, shortened) = mailto(Kind::Problem, message, Some(&details()));
        assert!(!shortened);
        assert!(url.starts_with("mailto:hello@sideband.studio?subject=EternalMonitor%20"));
        let params = url.split_once('?').unwrap().1;
        // Only unreserved characters and escapes after the ?, so no mail
        // app splits the body at a & or # inside the message.
        assert!(
            params
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.~%=&".contains(c)),
            "{params}"
        );
        assert_eq!(params.matches('&').count(), 1);
        assert!(url.contains("%0D%0A"));
        assert!(!url.contains('+') && !url.contains(' '));
        assert_eq!(
            query(&url, "body"),
            body(Kind::Problem, message, Some(&details())).replace('\n', "\r\n")
        );
        assert_eq!(query(&url, "subject"), subject(Kind::Problem));
    }

    #[test]
    fn a_long_message_is_cut_to_fit_and_keeps_the_details() {
        for unit in ["word ", "é", "画", "🙂"] {
            let message = unit.repeat(3000);
            let (url, shortened) = mailto(Kind::Problem, &message, Some(&details()));
            assert!(shortened, "{unit}");
            assert!(url.len() <= MAILTO_LIMIT, "{unit}: {}", url.len());
            // Not cut too early either: within one character's encoding.
            assert!(url.len() > MAILTO_LIMIT - 40, "{unit}: {}", url.len());
            let body = query(&url, "body");
            assert!(body.contains(SHORTENED), "{unit}");
            assert!(body.contains("Graphics: NVIDIA GeForce RTX 5080"), "{unit}");
            assert!(body.contains("Setup: Second screen"), "{unit}");
            assert!(body.starts_with(unit.trim()), "{unit}");
        }
        // Copy message always has the whole text.
        let message = "x".repeat(5000);
        assert!(full_text(Kind::Problem, &message, Some(&details())).contains(&message));
        let (url, shortened) = mailto(Kind::Problem, "short", Some(&details()));
        assert!(!shortened && !query(&url, "body").contains(SHORTENED));
    }

    #[test]
    fn copy_message_names_the_address_and_subject() {
        let text = full_text(Kind::Idea, "More frame rates", None);
        assert!(text.starts_with(&format!(
            "To: hello@sideband.studio\nSubject: {}\n\nMore frame rates",
            subject(Kind::Idea)
        )));
        let logs = with_logs(&text, "line one\nline two\n\n");
        assert!(logs.starts_with(&text));
        assert!(logs.ends_with("-- EternalMonitor log --\nline one\nline two"));
    }

    #[test]
    fn windows_version_reads_as_a_name_release_and_build() {
        assert_eq!(
            windows_label("24H2", "26100", "2894").as_deref(),
            Some("Windows 11 24H2 (build 26100.2894)")
        );
        assert_eq!(
            windows_label("22H2", "19045", "5011").as_deref(),
            Some("Windows 10 22H2 (build 19045.5011)")
        );
        assert_eq!(
            windows_label("unavailable(2)", "19041", "unavailable(2)").as_deref(),
            Some("Windows 10 (build 19041)")
        );
        assert_eq!(windows_label("24H2", "unavailable(2)", "1"), None);
    }

    #[test]
    fn the_ask_comes_after_three_sessions_of_five_minutes() {
        let mut ask = Ask::default();
        let minutes = |m: u64| Duration::from_secs(m * 60);
        // Short sessions never count.
        assert!(!ask.observe(Some((1, minutes(4)))));
        assert!(!ask.observe(Some((1, Duration::from_secs(299)))));
        // Five minutes counts once, however long the session goes on.
        assert!(ask.observe(Some((1, minutes(5)))));
        assert!(!ask.observe(Some((1, minutes(30)))));
        assert_eq!(ask.long_sessions, 1);
        assert!(!ask.observe(None));
        assert!(ask.observe(Some((2, minutes(6)))));
        assert!(!ask.due(false));
        assert!(ask.observe(Some((3, minutes(5)))));
        assert_eq!(ask.long_sessions, ASK_AFTER);
        // Never during a stream; only once the iPad is gone.
        assert!(!ask.due(true));
        assert!(ask.due(false));
        // More sessions don't grow the saved count.
        assert!(!ask.observe(Some((4, minutes(9)))));
        assert_eq!(ask.long_sessions, ASK_AFTER);
    }

    #[test]
    fn once_answered_the_ask_never_returns() {
        let mut ask = Ask::new(ASK_AFTER, false);
        assert!(ask.due(false));
        ask.finish();
        assert!(!ask.due(false));
        assert!(!ask.observe(Some((9, LONG_SESSION))));
        // As loaded from the settings file after a restart.
        let reloaded = Ask::new(ASK_AFTER, true);
        assert!(!reloaded.due(false));
        // A count saved before the restart carries on.
        let mut resumed = Ask::new(2, false);
        assert!(!resumed.due(false));
        assert!(resumed.observe(Some((1, LONG_SESSION))));
        assert!(resumed.due(false));
    }
}
