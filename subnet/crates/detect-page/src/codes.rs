//! The closed value sets of the contracts, each with one stable wire string.
//!
//! Every enum here serializes as its string and parses only that string, so
//! the JSON contracts, the D1 `CHECK` lists and the templates share one
//! spelling. The migration test checks the D1 lists against these sets.

/// Defines a code enum: its wire strings, `ALL`, `as_str`, `parse`, serde and
/// `Display`. Variants are ordered as declared. Exported for the sibling
/// contract crates; the caller depends on `serde`.
#[doc(hidden)]
#[macro_export]
macro_rules! code_enum {
    ($(#[$m:meta])* pub enum $name:ident { $($(#[$vm:meta])* $var:ident = $s:literal,)+ }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name { $($(#[$vm])* $var,)+ }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$var,)+];
            /// The wire string.
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$var => $s,)+ }
            }
            /// The value of a wire string; nothing else parses.
            pub fn parse(s: &str) -> Option<$name> {
                match s { $($s => Some($name::$var),)+ _ => None }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <String as ::serde::Deserialize>::deserialize(d)?;
                $name::parse(&s).ok_or_else(|| ::serde::de::Error::unknown_variant(&s, &[$($s,)+]))
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

code_enum! {
    /// The evidence band of a text of 150 words and more (the built detect
    /// Worker's codes). A short text has a [`ShortResult`] instead.
    pub enum Band {
        /// Band 1, "No strong signs" (confidence under 50%).
        Weak = "weak",
        /// Band 2, "Unclear" (50% to 90.9%).
        Unclear = "unclear",
        /// Band 3, "Strong" (90.9% to 99.988%).
        Strong = "strong",
        /// Band 4, "Very strong" (99.988% and up).
        VeryStrong = "very_strong",
    }
}

impl Band {
    /// Bands 3 and 4: passages may be highlighted.
    pub fn is_strong(self) -> bool {
        matches!(self, Band::Strong | Band::VeryStrong)
    }

    /// The band's number on the four-segment scale.
    pub fn number(self) -> u8 {
        self as u8 + 1
    }
}

code_enum! {
    /// The result of a short text (24 to 149 words): no percentages.
    pub enum ShortResult {
        /// "Strong signs of AI involvement."
        Strong = "strong",
        /// "No strong signs of AI involvement."
        None = "none",
    }
}

code_enum! {
    /// Where the scored words sit against the measured ranges (built codes).
    pub enum LengthBin {
        Short24 = "24_49",
        Short50 = "50_99",
        Short100 = "100_149",
        Measured = "150_450",
        Long = "451_1000",
    }
}

impl LengthBin {
    pub fn is_short(self) -> bool {
        matches!(self, LengthBin::Short24 | LengthBin::Short50 | LengthBin::Short100)
    }

    /// The bin of a scored word count, `None` under 24 words (no score) or
    /// over the free check's 1,000.
    pub fn of_words(words: u32) -> Option<LengthBin> {
        Some(match words {
            0..=23 => return None,
            24..=49 => LengthBin::Short24,
            50..=99 => LengthBin::Short50,
            100..=149 => LengthBin::Short100,
            150..=450 => LengthBin::Measured,
            451..=1000 => LengthBin::Long,
            _ => return None,
        })
    }
}

code_enum! {
    /// How the scored part of a free check was chosen (built codes).
    pub enum CutKind {
        /// The whole text was scored.
        None = "none",
        /// The first 1,000 words, cut back to a sentence end.
        FirstWords = "first_1000_words",
        /// Cut further to the scorer's token limit.
        TokenLimit = "token_limit",
    }
}

code_enum! {
    /// What a page is.
    pub enum Kind {
        /// A public URL check, `/d/<7>` on the site host.
        PublicUrl = "public_url",
        /// A secret link of pasted text, `/<20>` on the secret host.
        PrivateText = "private_text",
        /// A paid whole-document check, a secret link.
        PrivatePaid = "private_paid",
        /// The result of a free check before any link exists (never stored).
        Unstored = "unstored",
        /// A synthetic example report on the tool page.
        Example = "example",
    }
}

impl Kind {
    /// The kinds a `detections` row can hold.
    pub const STORED: &'static [Kind] = &[Kind::PublicUrl, Kind::PrivateText, Kind::PrivatePaid];

    pub fn is_secret(self) -> bool {
        matches!(self, Kind::PrivateText | Kind::PrivatePaid)
    }
}

code_enum! {
    /// Where the text came from (`detections.platform`). Pasted text is
    /// `paste` whatever source the submitter names, an X host included.
    pub enum Platform {
        Paste = "paste",
        Web = "web",
        Bluesky = "bluesky",
        /// A post on X checked from its link: a public page that keeps no
        /// post text and shows none, only our result and a link to the post
        /// ([`crate::x`]).
        X = "x",
    }
}

code_enum! {
    /// A detection's lifecycle state (`detections.state`).
    pub enum DetState {
        Pending = "pending",
        Live = "live",
        Failed = "failed",
        ExcerptWithheld = "excerpt_withheld",
        Removed = "removed",
        Disabled = "disabled",
        Deleted = "deleted",
        Expired = "expired",
    }
}

impl DetState {
    /// States whose page shows nothing but its status (410 or 451).
    pub fn is_ended(self) -> bool {
        matches!(self, DetState::Removed | DetState::Disabled | DetState::Deleted | DetState::Expired)
    }
}

code_enum! {
    /// Which public host a request arrived on, or a page belongs to. The
    /// host names come from config ([`crate::Origins`]).
    pub enum HostRole {
        /// The site host (from `SITE_ORIGIN`).
        Site = "site",
        /// The secret host (from `SECRET_ORIGIN`).
        Secret = "secret",
    }
}

code_enum! {
    /// A page's robots directive.
    pub enum Robots {
        Index = "index",
        Noindex = "noindex",
        /// The secret host's full set.
        NoindexAll = "noindex_all",
    }
}

impl Robots {
    /// The `X-Robots-Tag` header and robots meta value.
    pub fn header_value(self) -> &'static str {
        match self {
            Robots::Index => "index",
            Robots::Noindex => "noindex",
            Robots::NoindexAll => "noindex, nofollow, noarchive, nosnippet, noimageindex",
        }
    }
}

code_enum! {
    /// Why no passage is drawn (the built Worker's codes, and `x_post`).
    pub enum SpansNote {
        StrongBandsOnly = "strong_bands_only",
        Off = "off",
        Unavailable = "unavailable",
        /// A post on X: we keep and show none of the post's text, so no
        /// passage is drawn at any length ("Passages aren't highlighted on
        /// posts from X: we don't show the post's text." on 150 words and
        /// more).
        XPost = "x_post",
    }
}

code_enum! {
    /// The ninja's state, ordered `Bow < Stoic < Ready < Strike`.
    pub enum NinjaState {
        Bow = "bow",
        Stoic = "stoic",
        Ready = "ready",
        Strike = "strike",
    }
}

code_enum! {
    /// A share card's layout.
    pub enum CardVariant {
        /// A text of 150 words and more: the band card.
        Band = "band",
        /// A short text: the result, no percentages.
        Short = "short",
        /// A paid whole document: the neutral dial and the section ladder.
        PaidDocument = "paid_document",
        /// The band card without the ninja (the cards-generic switch, or a
        /// withheld excerpt).
        Generic = "generic",
    }
}

code_enum! {
    /// A public source's standing in the ranking screen.
    pub enum Reputation {
        Ranked = "ranked",
        Unranked = "unranked",
        Adult = "adult",
    }
}

code_enum! {
    /// A public source's state (`sources.state`).
    pub enum SourceStatus {
        Ok = "ok",
        Gone = "gone",
        OptedOut = "opted_out",
        Unlisted = "unlisted",
    }
}

code_enum! {
    /// How a source was read.
    pub enum SourceVia {
        Web = "web",
        Bluesky = "bluesky",
        X = "x",
        /// Named by the person who pasted the text; not checked.
        Submitter = "submitter",
    }
}

code_enum! {
    /// The fetch adapter of a source (`sources.adapter`).
    pub enum Adapter {
        Web = "web",
        Bluesky = "bluesky",
        X = "x",
    }
}

code_enum! {
    /// How much of the text a page shows.
    pub enum Shown {
        Full = "full",
        Excerpt = "excerpt",
        Withheld = "withheld",
    }
}

code_enum! {
    /// Why text is withheld.
    pub enum WithheldReason {
        LegalNotice = "legal_notice",
        SourceGone = "source_gone",
        Unlisted = "unlisted",
        Unranked = "unranked",
    }
}

code_enum! {
    /// What a range of a public page's quoted body is.
    pub enum AnchorRole {
        /// A highlighted passage.
        Passage = "passage",
        /// The greyed context sentence beside a passage.
        Context = "context",
        /// The first sentence of a band 1 or 2 public page, set large.
        FirstSentence = "first_sentence",
    }
}

code_enum! {
    /// A secret link's lifetime.
    pub enum Expiry {
        Day = "1d",
        Week = "7d",
        Month = "30d",
        /// Until deleted (the default).
        Keep = "keep",
    }
}

impl Expiry {
    /// Milliseconds from now, `None` for [`Expiry::Keep`].
    pub fn duration_ms(self) -> Option<u64> {
        const DAY: u64 = 86_400_000;
        match self {
            Expiry::Day => Some(DAY),
            Expiry::Week => Some(7 * DAY),
            Expiry::Month => Some(30 * DAY),
            Expiry::Keep => None,
        }
    }
}

code_enum! {
    /// What an owner purge does to an account's pages (by default:
    /// delete).
    pub enum PurgeMode {
        Delete = "delete",
        /// Keep them as anonymous secret links.
        Release = "release",
    }
}

code_enum! {
    /// A removal case's kind, and the form's track (`cases.kind`).
    pub enum CaseKind {
        Copyright = "copyright",
        /// "I wrote or published this".
        Author = "author",
        PersonalData = "personal_data",
        WrongResult = "wrong_result",
        Illegal = "illegal",
        CounterNotice = "counter_notice",
        CourtOrder = "court_order",
        OtherLegal = "other_legal",
        /// A removal at the request of an X post's author or of X that came
        /// by mail or on X (op `x_remove`), or an operator `remove` of an X
        /// page outside a case: content-free, with no notice document and
        /// no status token. Never a form track: intake refuses it.
        XRequest = "x_request",
    }
}

impl CaseKind {
    /// Whether the removal form may take this kind as its track.
    pub fn is_form_track(self) -> bool {
        self != CaseKind::XRequest
    }
}

code_enum! {
    /// A removal case's state (`cases.state`).
    pub enum CaseState {
        Received = "received",
        Incomplete = "incomplete",
        Reviewing = "reviewing",
        Accepted = "accepted",
        Countered = "countered",
        Restored = "restored",
        Rejected = "rejected",
        Closed = "closed",
    }
}

code_enum! {
    /// What a removal took away.
    pub enum RemovalScope {
        Excerpt = "excerpt",
        Page = "page",
    }
}

code_enum! {
    /// How a page came to have an owner (`detections.owner_via`).
    pub enum OwnerVia {
        Created = "created",
        Paid = "paid",
        Claimed = "claimed",
    }
}

code_enum! {
    /// A page's owner as seen by the asking account.
    pub enum OwnerRelation {
        You = "you",
        Other = "other",
        None = "none",
    }
}

code_enum! {
    /// A scan's counting channel (`hour_counters.channel`).
    pub enum Channel {
        FreeText = "free_text",
        FreeUrl = "free_url",
        PaidPage = "paid_page",
        Pro = "pro",
        Api = "api",
        Operator = "operator",
    }
}

code_enum! {
    /// A gate ticket's class.
    pub enum TicketClass {
        Interactive = "interactive",
        Url = "url",
        Paid = "paid",
        Background = "background",
    }
}

code_enum! {
    /// A paid job's state (`jobs.state`).
    pub enum JobState {
        Running = "running",
        Scored = "scored",
        Committed = "committed",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

code_enum! {
    /// The operator's index override (`detections.index_force`).
    pub enum IndexForce {
        On = "on",
        Off = "off",
    }
}

code_enum! {
    /// Where an example slot's report comes from (`examples.source`).
    pub enum ExampleSource {
        Synthetic = "synthetic",
        Public = "public",
    }
}

code_enum! {
    /// The operator's review of a strike on a claimed page (`strikes.review`).
    pub enum StrikeReview {
        Counts = "counts",
        NotCounted = "not_counted",
    }
}

code_enum! {
    /// A manage request's action.
    pub enum ManageAction {
        Delete = "delete",
        SetExpiry = "set_expiry",
        ClaimCheck = "claim_check",
    }
}

code_enum! {
    /// An owner request's action (`/internal/v1/owner`).
    pub enum OwnerAction {
        Peek = "peek",
        Claim = "claim",
        Delete = "delete",
        Release = "release",
    }
}

code_enum! {
    /// A removal form step.
    pub enum RemovalStep {
        Review = "review",
        Send = "send",
    }
}

code_enum! {
    /// An IndexNow change.
    pub enum IndexChange {
        Updated = "updated",
        Deleted = "deleted",
    }
}

code_enum! {
    /// The `BILLING` switch's states.
    pub enum BillingState {
        Off = "off",
        Grants = "grants",
        Live = "live",
    }
}

code_enum! {
    /// The `DETECT_X` switch's states.
    pub enum DetectXState {
        /// The default: post links answer `x_off`; existing X pages are still
        /// served, checked and deleted.
        Off = "off",
        On = "on",
        /// Set by `x_purge` (and by `set_switch`, so the operator can always
        /// leave it): every X page answers 410 `x_purged` and every post link
        /// `x_off`, while the purge pages delete the rows.
        Purged = "purged",
    }
}

code_enum! {
    /// Why an X source is being deleted, carried by `x_delete`
    /// ([`crate::queue::Job::XDelete`]) when the deletion stopped between
    /// its R2 delete and its D1 batch. Each cause is the refusal code its
    /// detections end with ([`XDeleteCause::code`]).
    pub enum XDeleteCause {
        /// X's embed no longer shows the post (one `gone` answer of the
        /// daily existence check, after both controls passed).
        XGone = "x_gone",
        /// A removal at the request of the post's author or of X: the
        /// detections are removed and the source keeps only its key.
        XRemoved = "x_removed",
    }
}

impl XDeleteCause {
    /// The refusal code the source's detections end with, also their
    /// [`crate::page::Ended`] reason.
    pub fn code(self) -> crate::refusal::RefusalCode {
        use crate::refusal::RefusalCode as R;
        match self {
            XDeleteCause::XGone => R::XGone,
            XDeleteCause::XRemoved => R::XRemoved,
        }
    }

    /// The state the source's detections end in: `removed` for a removal,
    /// `deleted` otherwise.
    pub fn state(self) -> DetState {
        match self {
            XDeleteCause::XRemoved => DetState::Removed,
            _ => DetState::Deleted,
        }
    }

    /// Whether the source row goes (every cause but a removal, which keeps
    /// the row as a tombstone holding only its key).
    pub fn deletes_source_row(self) -> bool {
        self != XDeleteCause::XRemoved
    }
}
