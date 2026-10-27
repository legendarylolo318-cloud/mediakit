//! The single status dot: folds ffmpeg, yt-dlp, Node.js and the PO-token
//! server into one colour plus a per-component breakdown for the tooltip and
//! popup. Pure logic so the colour rules are unit-tested.

use mediakit_core::pot_provider::PotProviderState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Everything needed is ready.
    Green,
    /// Starting, or degraded but usable (e.g. YouTube without a PO token).
    Yellow,
    /// ffmpeg missing - the app can't do its main job.
    Red,
    /// Idle: the PO-token server is enabled but hasn't been needed yet.
    Grey,
}

pub struct StatusInputs {
    pub ffmpeg_version: Option<String>,
    pub ytdlp_version: Option<String>,
    /// `None` while the background Node probe is still running.
    pub node_available: Option<bool>,
    pub pot_enabled: bool,
    pub pot_state: PotProviderState,
}

pub struct Component {
    pub name: &'static str,
    pub detail: String,
}

pub fn health(i: &StatusInputs) -> Health {
    if i.ffmpeg_version.is_none() {
        return Health::Red;
    }
    // No yt-dlp means no downloads at all; no Node means YouTube runs
    // without a JS runtime (and so without a PO token) - degraded whether
    // or not the PO-token server is enabled.
    if i.ytdlp_version.is_none() || i.node_available == Some(false) {
        return Health::Yellow;
    }
    if !i.pot_enabled {
        return Health::Green;
    }
    match &i.pot_state {
        PotProviderState::Starting | PotProviderState::Failed(_) => Health::Yellow,
        PotProviderState::Ready { .. } => Health::Green,
        PotProviderState::Stopped => Health::Grey,
    }
}

pub fn components(i: &StatusInputs) -> Vec<Component> {
    let tool = |name, v: &Option<String>, missing: &str| Component {
        name,
        detail: v.clone().unwrap_or_else(|| missing.to_string()),
    };
    let node = match i.node_available {
        Some(true) => "found".to_string(),
        Some(false) => {
            "not found - YouTube downloads may fail (no JS runtime or PO token)".to_string()
        }
        None => "checking\u{2026}".to_string(),
    };
    let pot = if !i.pot_enabled {
        "disabled in Settings".to_string()
    } else {
        match &i.pot_state {
            PotProviderState::Stopped => "idle (starts on the first YouTube link)".to_string(),
            PotProviderState::Starting => "starting\u{2026}".to_string(),
            PotProviderState::Ready { port } => format!("ready on port {port}"),
            PotProviderState::Failed(reason) => format!("failed: {reason}"),
        }
    };
    vec![
        tool("ffmpeg", &i.ffmpeg_version, "not found"),
        tool("yt-dlp", &i.ytdlp_version, "not found"),
        Component {
            name: "Node.js",
            detail: node,
        },
        Component {
            name: "PO-token server",
            detail: pot,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> StatusInputs {
        StatusInputs {
            ffmpeg_version: Some("8.1".into()),
            ytdlp_version: Some("2026.10.26".into()),
            node_available: Some(true),
            pot_enabled: true,
            pot_state: PotProviderState::Ready { port: 4416 },
        }
    }

    #[test]
    fn all_ready_is_green() {
        assert_eq!(health(&inputs()), Health::Green);
    }

    #[test]
    fn missing_ffmpeg_is_red_whatever_else_is_true() {
        let mut i = inputs();
        i.ffmpeg_version = None;
        assert_eq!(health(&i), Health::Red);
    }

    #[test]
    fn starting_failed_and_node_missing_are_yellow() {
        let mut i = inputs();
        i.pot_state = PotProviderState::Starting;
        assert_eq!(health(&i), Health::Yellow);
        i.pot_state = PotProviderState::Failed("x".into());
        assert_eq!(health(&i), Health::Yellow);
        i = inputs();
        i.node_available = Some(false);
        assert_eq!(health(&i), Health::Yellow);
    }

    #[test]
    fn node_or_ytdlp_missing_is_yellow_even_with_the_server_disabled() {
        let mut i = inputs();
        i.pot_enabled = false;
        i.node_available = Some(false);
        assert_eq!(health(&i), Health::Yellow);
        i = inputs();
        i.pot_enabled = false;
        i.ytdlp_version = None;
        assert_eq!(health(&i), Health::Yellow);
    }

    #[test]
    fn idle_server_is_grey_and_disabled_server_is_green() {
        let mut i = inputs();
        i.pot_state = PotProviderState::Stopped;
        assert_eq!(health(&i), Health::Grey);
        i.pot_enabled = false;
        assert_eq!(health(&i), Health::Green);
    }

    #[test]
    fn components_list_every_part() {
        let names: Vec<_> = components(&inputs()).iter().map(|c| c.name).collect();
        assert_eq!(names, ["ffmpeg", "yt-dlp", "Node.js", "PO-token server"]);
    }
}
