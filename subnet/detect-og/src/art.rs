//! The art a card draws, embedded at build time from `assets/brand/`: the
//! four ninja states, the gauge's two end busts and the dark-background
//! wordmark. No card ever references an external file or URL; each drawing
//! is inlined into the card's SVG as a nested `<svg>` with its own
//! `viewBox` ([`Art::nested`]).

use slop_ninja_detect_page::NinjaState;

/// One embedded drawing.
#[derive(Clone, Copy, Debug)]
pub struct Art {
    pub name: &'static str,
    /// The file, as committed.
    pub file: &'static str,
    /// Its root `viewBox`.
    pub view_box: &'static str,
}

pub const STRIKE: Art =
    Art { name: "ninja/strike", file: include_str!("../../../assets/brand/ninja/strike.svg"), view_box: "0 0 913 913" };
pub const READY: Art =
    Art { name: "ninja/ready", file: include_str!("../../../assets/brand/ninja/ready.svg"), view_box: "0 0 913 913" };
pub const STOIC: Art =
    Art { name: "ninja/stoic", file: include_str!("../../../assets/brand/ninja/stoic.svg"), view_box: "0 0 913 913" };
pub const BOW: Art =
    Art { name: "ninja/bow", file: include_str!("../../../assets/brand/ninja/bow.svg"), view_box: "0 0 913 913" };
pub const HUMAN: Art =
    Art { name: "gauge/human", file: include_str!("../../../assets/brand/gauge/human.svg"), view_box: "0 0 880 880" };
pub const ROBOT: Art =
    Art { name: "gauge/robot", file: include_str!("../../../assets/brand/gauge/robot.svg"), view_box: "0 0 880 880" };
pub const WORDMARK: Art = Art {
    name: "slop-ninja-wordmark",
    file: include_str!("../../../assets/brand/slop-ninja-wordmark.svg"),
    view_box: "151 853 952 208",
};

/// Every embedded drawing.
pub const ALL: [Art; 7] = [STRIKE, READY, STOIC, BOW, HUMAN, ROBOT, WORDMARK];

/// The drawing of a ninja state.
pub fn ninja(state: NinjaState) -> Art {
    match state {
        NinjaState::Strike => STRIKE,
        NinjaState::Ready => READY,
        NinjaState::Stoic => STOIC,
        NinjaState::Bow => BOW,
    }
}

/// Where a state's soles meet the ground, as fractions of the 913 px box:
/// the stance's middle and width (where the contact shadow sits).
pub fn sole(state: NinjaState) -> (f32, f32) {
    match state {
        NinjaState::Strike => (0.485, 0.34),
        NinjaState::Ready => (0.486, 0.32),
        NinjaState::Stoic => (0.566, 0.24),
        NinjaState::Bow => (0.553, 0.22),
    }
}

impl Art {
    /// The drawing's content: everything between its root `<svg …>` start
    /// tag and the closing `</svg>` (an XML declaration before the root is
    /// dropped).
    pub fn inner(&self) -> Option<&'static str> {
        let open = self.file.find("<svg")?;
        let body = open + self.file.get(open..)?.find('>')? + 1;
        let close = self.file.trim_end().strip_suffix("</svg>")?.len();
        self.file.get(body..close)
    }

    /// The drawing as a nested `<svg>` at `x`, `y`, `w` by `h` pixels.
    pub fn nested(&self, out: &mut String, x: f32, y: f32, w: f32, h: f32) -> Option<()> {
        use crate::svg::{N, put};
        let inner = self.inner()?;
        put(
            out,
            format_args!(
                r#"<svg x="{}" y="{}" width="{}" height="{}" viewBox="{}" preserveAspectRatio="xMidYMid meet">"#,
                N(x),
                N(y),
                N(w),
                N(h),
                self.view_box
            ),
        );
        out.push_str(inner);
        out.push_str("</svg>");
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_drawing_is_self_contained() {
        for a in ALL {
            let inner = a.inner().unwrap_or_else(|| panic!("{}: no root element", a.name));
            let root = &a.file[a.file.find("<svg").unwrap()..];
            assert!(root[..root.find('>').unwrap()].contains(&format!("viewBox=\"{}\"", a.view_box)), "{}", a.name);
            // Nothing the renderer would have to fetch or run.
            for bad in
                ["<image", "<script", "<foreignObject", "href=\"http", "href=\"//", "url(http", "@import", "<use"]
            {
                assert!(!inner.contains(bad), "{}: {bad}", a.name);
            }
            assert!(!inner.contains("<svg"), "{}: a nested root", a.name);
        }
    }
}
