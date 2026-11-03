//! Blog-register helpers shared by the FineWeb blog adapter and the blog
//! split step: URL classification into a blog identity, a first-person
//! screen and a counts-only register lexicon.
//!
//! * **Blog identity.** A page is a blog page when its URL matches one rule
//!   (first match wins, in this order):
//!   1. `blogspot`: the registered domain (Public Suffix List, which lists
//!      `blogspot.*` in its private section) is `<name>.blogspot.<tld>`.
//!   2. `hosted_platform`: the registered domain is a multi-tenant blog or
//!      journal platform that the list does not separate (`PLATFORMS`); the
//!      blog is the user subdomain, or for path-hosted platforms the first
//!      path segment(s) (`medium.com/@user`, `livejournal.com/users/x`,
//!      `xanga.com/user`). Service pages without a blog identity are not
//!      blog pages.
//!   3. `blog_subdomain`: the host's first label is `blog`, `blogs`,
//!      `weblog` or `journal`.
//!   4. `blog_path`: a path segment is `blog`, `blogs`, `weblog` or
//!      `weblogs`.
//!   5. `blogger_permalink`: the path is Blogger's `/YYYY/MM/<slug>.html`
//!      (custom-domain Blogger blogs).
//!   6. `dated_slug_permalink`: the path is `/YYYY/MM/<slug>/` or
//!      `/YYYY/MM/DD/<slug>/` with a hyphenated slug (the default
//!      WordPress permalink; also used by some news sites, so it is
//!      reported separately).
//!   7. `blog_in_domain_name`: the registered domain's first label contains
//!      `blog`.
//! * **Keys.** Hosted-platform blogs are keyed `fineweb_blog:<blog id>`;
//!   every other blog page keeps FineWeb's own split group
//!   `fineweb_site:<registered domain>`, so it can never disagree with the
//!   FineWeb pool's split of that site.
use crate::{psl::Psl, text};
use serde_json::{Value, json};

/// Multi-tenant blog and journal platforms not separated by the Public
/// Suffix List: (registered domain, platform label, path-hosted).
pub const PLATFORMS: &[(&str, &str)] = &[
    ("wordpress.com", "wordpress_com"),
    ("livejournal.com", "livejournal"),
    ("dreamwidth.org", "dreamwidth"),
    ("insanejournal.com", "insanejournal"),
    ("typepad.com", "typepad"),
    ("blogs.com", "typepad_blogs_com"),
    ("tumblr.com", "tumblr"),
    ("xanga.com", "xanga"),
    ("medium.com", "medium"),
    ("substack.com", "substack"),
    ("weebly.com", "weebly"),
    ("over-blog.com", "over_blog"),
    ("edublogs.org", "edublogs"),
    ("blog.com", "blog_com"),
    ("blogsome.com", "blogsome"),
    ("ghost.io", "ghost"),
    ("posterous.com", "posterous"),
    ("vox.com", "vox"),
];

/// Subdomain labels of a platform that are service hosts, not blogs.
pub const SERVICE_LABELS: &[&str] = &[
    "www", "m", "mobile", "en", "api", "support", "help", "about", "login", "signup", "status", "developer", "developers",
    "media", "static", "s", "s0", "s1", "s2", "i", "i0", "i1", "i2", "files", "assets", "cdn", "img", "images", "mail", "shop",
    "store", "jobs", "press", "forums", "forum", "stat", "l-stat", "pics", "staff", "en-gb", "public-api", "dashboard",
    "subscribe", "wordpress", "theme", "themes", "plugins", "apps", "go", "learn", "faq", "users", "community",
];

/// First path segments of path-hosted platforms that are service pages.
pub const SERVICE_PATHS: &[&str] = &[
    "", "p", "m", "tag", "tags", "topic", "topics", "search", "me", "about", "policy", "membership", "creators", "plans",
    "login", "signup", "settings", "help", "jobs", "tagged", "explore", "feed", "rss", "media", "static", "browse",
    "interests", "lists", "notifications", "hc", "legal", "privacy", "terms", "blog", "home", "index.html", "index.php",
];

#[derive(Debug, Clone, PartialEq)]
pub struct BlogUrl {
    pub rule: &'static str,
    pub platform: &'static str,
    /// The blog identity (`<name>.wordpress.com`, `medium.com/@x`, or the
    /// registered domain for non-hosted blogs).
    pub blog_id: String,
    pub hosted: bool,
}

impl BlogUrl {
    /// Group key before any inheritance (see the module documentation).
    pub fn key(&self, registered_domain: &str) -> String {
        if self.hosted { format!("fineweb_blog:{}", self.blog_id) } else { format!("fineweb_site:{registered_domain}") }
    }
}

fn path_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let p = rest.find('/').map(|i| &rest[i..]).unwrap_or("/");
    p.split(['?', '#']).next().unwrap_or("/")
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

fn year_month(a: &str, b: &str) -> bool {
    digits(a, 4) && (a.starts_with("19") || a.starts_with("20")) && digits(b, 2) && ("01"..="12").contains(&b)
}

fn slug_like(s: &str) -> bool {
    s.contains('-') && s.len() >= 8 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'%')
}

pub fn blogger_permalink(path: &str) -> bool {
    let s = segments(path);
    s.len() == 3 && year_month(s[0], s[1]) && s[2].strip_suffix(".html").is_some_and(|x| x.len() >= 3)
}

pub fn dated_slug_permalink(path: &str) -> bool {
    let s = segments(path);
    match s.len() {
        3 => year_month(s[0], s[1]) && slug_like(s[2]),
        4 => year_month(s[0], s[1]) && digits(s[2], 2) && slug_like(s[3]),
        _ => false,
    }
}

fn hosted_blog(platform_domain: &str, host: &str, path: &str) -> Option<String> {
    let host = host.trim_end_matches('.');
    let sub = host.strip_suffix(platform_domain).and_then(|h| h.strip_suffix('.'));
    let seg = segments(path);
    let first = seg.first().copied().unwrap_or("");
    // User subdomain (the last label before the platform domain).
    if let Some(sub) = sub {
        let label = sub.rsplit('.').next().unwrap_or(sub);
        if !SERVICE_LABELS.contains(&label) && !label.is_empty() {
            return Some(format!("{label}.{platform_domain}"));
        }
        // LiveJournal-style `users.` / `community.` hosts: /<name>/...
        if (label == "users" || label == "community") && !first.is_empty() && seg.len() >= 2 {
            return Some(format!("{label}.{platform_domain}/{}", first.to_lowercase()));
        }
    }
    let service_host = sub.is_none_or(|s| SERVICE_LABELS.contains(&s.rsplit('.').next().unwrap_or(s)));
    if !service_host {
        return None;
    }
    match platform_domain {
        "medium.com" => {
            if let Some(u) = first.strip_prefix('@').filter(|u| !u.is_empty()) {
                return Some(format!("medium.com/@{}", u.to_lowercase()));
            }
            (seg.len() >= 2 && !SERVICE_PATHS.contains(&first)).then(|| format!("medium.com/{}", first.to_lowercase()))
        }
        "livejournal.com" | "dreamwidth.org" | "insanejournal.com" => {
            if (first == "users" || first == "community" || first == "~") && seg.len() >= 3 {
                Some(format!("{platform_domain}/{first}/{}", seg[1].to_lowercase()))
            } else {
                None
            }
        }
        "xanga.com" => (seg.len() >= 2 && !SERVICE_PATHS.contains(&first)).then(|| format!("xanga.com/{}", first.to_lowercase())),
        _ => None,
    }
}

/// Classify a page URL. `host` is the lowercased URL host and `domain` its
/// registered domain.
pub fn classify_url(url: &str, host: &str, domain: &str) -> Option<BlogUrl> {
    let path = path_of(url);
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() >= 3 && labels[1] == "blogspot" {
        return Some(BlogUrl { rule: "blogspot", platform: "blogspot", blog_id: domain.to_owned(), hosted: false });
    }
    if let Some((pd, label)) = PLATFORMS.iter().find(|(d, _)| *d == domain) {
        return hosted_blog(pd, host, path).map(|b| BlogUrl { rule: "hosted_platform", platform: label, blog_id: b, hosted: true });
    }
    let own = |rule, platform| Some(BlogUrl { rule, platform, blog_id: domain.to_owned(), hosted: false });
    let first_label = host.split('.').next().unwrap_or("");
    if host != domain && ["blog", "blogs", "weblog", "journal"].contains(&first_label) {
        return own("blog_subdomain", "own_domain");
    }
    let seg = segments(path);
    if seg.iter().any(|s| ["blog", "blogs", "weblog", "weblogs"].contains(&s.to_lowercase().as_str())) {
        return own("blog_path", "own_domain");
    }
    if blogger_permalink(path) {
        return own("blogger_permalink", "blogger_custom_domain");
    }
    if dated_slug_permalink(path) {
        return own("dated_slug_permalink", "own_domain");
    }
    if labels.first().is_some_and(|l| l.contains("blog")) {
        return own("blog_in_domain_name", "own_domain");
    }
    None
}

/// Classify with the Public Suffix List; `None` when the URL has no usable
/// host or registered domain or is not a blog page.
pub fn classify_with(psl: &Psl, url: &str) -> Option<(String, String, BlogUrl)> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let host = crate::psl::url_host(url)?;
    let domain = psl.registered_domain(&host)?;
    let b = classify_url(url, &host, &domain)?;
    Some((host, domain, b))
}

/// Blogger country-domain blog: a host `<name>.blogspot.<suffix>` whose
/// registered domain is `blogspot.<suffix>` itself (the Public Suffix List in
/// use no longer lists that country suffix). Returns `<name>`; Blogger serves
/// one blog under every country domain, so the blog is `<name>.blogspot.com`.
pub fn blogspot_country_blog<'a>(host: &'a str, domain: &str) -> Option<&'a str> {
    if !domain.starts_with("blogspot.") || domain.len() <= "blogspot.".len() {
        return None;
    }
    let sub = host.trim_end_matches('.').strip_suffix(domain)?.strip_suffix('.')?;
    let label = sub.rsplit('.').next().unwrap_or(sub);
    (!label.is_empty() && !SERVICE_LABELS.contains(&label)).then_some(label)
}

/// Writer identity of a page URL for writer keys and contributor closure:
/// the blog itself, never a whole multi-tenant platform. Returns the key
/// and the rule that produced it:
/// * `hosted_blog`: a `PLATFORMS` domain with a blog identity
///   (`fineweb_blog:<user subdomain or first path segment(s)>`);
/// * `blogspot_country_domain`: `fineweb_site:<name>.blogspot.com` for
///   `<name>.blogspot.<country suffix>` (see `blogspot_country_blog`);
/// * `registered_domain`: `fineweb_site:<registered domain>` otherwise,
///   including platform service pages without a blog identity.
pub fn writer_identity(psl: &Psl, url: &str) -> Option<(String, &'static str)> {
    let host = crate::psl::url_host(url)?;
    let domain = psl.registered_domain(&host)?;
    if let Some(name) = blogspot_country_blog(&host, &domain) {
        return Some((format!("fineweb_site:{name}.blogspot.com"), "blogspot_country_domain"));
    }
    if let Some((pd, _)) = PLATFORMS.iter().find(|(d, _)| *d == domain) {
        if let Some(b) = hosted_blog(pd, &host, path_of(url)) {
            return Some((format!("fineweb_blog:{b}"), "hosted_blog"));
        }
    }
    Some((format!("fineweb_site:{domain}"), "registered_domain"))
}

/// Lowercase line fragments of blog page chrome (comment headers, labels,
/// post navigation, profile and sharing widgets); a line with one is a
/// paragraph break.
pub const BLOG_CHROME: &[&str] = &[
    " said...", " said\u{2026}", "labels:", "tags:", "posted in", "filed under", "permalink", "trackback", "pingback",
    "older post", "newer post", "previous post", "next post", "reblog", "no comments", " comments:", "share on", "pin it",
    "follow me", "view my complete profile", "blog archive", "related posts", "you might also like", "this entry was posted",
    "bookmark the", "categories:", "posted at", "written by", "about the author", "rss feed", "add your comment",
];

pub fn blog_chrome(line: &str) -> bool {
    let l = line.to_lowercase();
    BLOG_CHROME.iter().any(|b| l.contains(b))
}

pub const FIRST_PERSON_SINGULAR: &[&str] = &["i", "me", "my", "mine", "myself"];
pub const FIRST_PERSON_PLURAL: &[&str] = &["we", "us", "our", "ours", "ourselves"];

#[derive(Debug, Clone, Copy)]
pub struct FirstPersonRule {
    pub min_singular: usize,
    pub min_plural: usize,
    pub min_rate: f64,
}

pub const FIRST_PERSON_RULE: FirstPersonRule = FirstPersonRule { min_singular: 3, min_plural: 5, min_rate: 0.012 };

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FirstPerson {
    pub singular: usize,
    pub plural: usize,
    pub words: usize,
}

impl FirstPerson {
    pub fn of(t: &str) -> FirstPerson {
        let toks = text::folded_tokens(t);
        let singular = toks.iter().filter(|w| FIRST_PERSON_SINGULAR.contains(&w.as_str())).count();
        let plural = toks.iter().filter(|w| FIRST_PERSON_PLURAL.contains(&w.as_str())).count();
        FirstPerson { singular, plural, words: text::words(t) }
    }
    pub fn rate(&self) -> f64 {
        if self.words == 0 { 0.0 } else { (self.singular + self.plural) as f64 / self.words as f64 }
    }
    /// First-person prose: enough singular (or, for a shared "we" voice,
    /// plural) first-person forms at a minimum overall rate.
    pub fn passes(&self, r: FirstPersonRule) -> bool {
        (self.singular >= r.min_singular || self.plural >= r.min_plural) && self.rate() >= r.min_rate
    }
    pub fn voice(&self) -> &'static str {
        if self.singular >= self.plural { "first_person_singular" } else { "first_person_plural" }
    }
    pub fn to_value(&self) -> Value {
        json!({"singular": self.singular, "plural": self.plural, "words": self.words, "rate": (self.rate() * 10000.0).round() / 10000.0, "voice": self.voice()})
    }
}

/// Counts-only register lexicon: topic label = the category with the most
/// token hits (at least `REGISTER_MIN_HITS`, ties to the earlier category),
/// otherwise `general_personal`. It is a descriptive tally for coverage
/// reports, never a selection or training feature.
pub const REGISTERS: &[(&str, &[&str])] = &[
    ("food_recipes", &["recipe", "recipes", "cook", "cooking", "bake", "baking", "oven", "flour", "sugar", "butter", "delicious", "dinner", "lunch", "breakfast", "chocolate", "sauce", "cake", "cookies", "soup", "kitchen"]),
    ("family_parenting", &["kids", "son", "daughter", "baby", "husband", "wife", "children", "mom", "dad", "mommy", "toddler", "pregnant", "parenting", "grandma", "grandmother", "boys", "girls"]),
    ("faith_religion", &["god", "jesus", "church", "pray", "prayer", "faith", "bible", "lord", "christ", "spiritual", "worship", "scripture", "holy", "sermon", "gospel"]),
    ("crafts_diy_home", &["knit", "knitting", "sew", "sewing", "yarn", "fabric", "quilt", "crochet", "craft", "crafts", "paint", "painted", "diy", "stitch", "pattern", "decor", "furniture"]),
    ("garden_nature", &["garden", "gardening", "plants", "seeds", "flowers", "tomatoes", "birds", "trees", "hike", "hiking", "trail", "camping", "fishing", "weather", "snow"]),
    ("travel", &["trip", "travel", "hotel", "flight", "beach", "visit", "visited", "tour", "airport", "vacation", "museum", "island", "road", "drove"]),
    ("tech_computing", &["software", "code", "computer", "app", "windows", "linux", "server", "google", "phone", "iphone", "install", "website", "browser", "programming", "android", "laptop", "online"]),
    ("politics_society", &["government", "president", "election", "policy", "vote", "obama", "trump", "party", "congress", "law", "rights", "political", "democrats", "republicans", "war", "state", "tax"]),
    ("books_film_music", &["book", "books", "novel", "author", "film", "movie", "movies", "music", "album", "song", "songs", "band", "episode", "character", "characters", "story", "series", "poem", "poetry"]),
    ("health_fitness", &["run", "running", "weight", "workout", "gym", "diet", "health", "doctor", "pain", "cancer", "surgery", "hospital", "exercise", "miles", "marathon", "body"]),
    ("business_marketing", &["business", "customers", "marketing", "company", "sales", "clients", "brand", "seo", "product", "products", "services", "customer", "market", "industry", "startup"]),
    ("sports_games", &["game", "games", "team", "season", "players", "coach", "league", "win", "won", "score", "football", "baseball", "basketball", "soccer", "hockey", "match"]),
    ("education_work", &["students", "student", "teacher", "teaching", "class", "school", "college", "university", "work", "job", "office", "career", "boss", "classroom"]),
    ("fashion_beauty", &["dress", "shoes", "outfit", "fashion", "makeup", "hair", "skin", "wear", "wearing", "style", "jewelry", "nails"]),
    ("pets_animals", &["dog", "dogs", "cat", "cats", "puppy", "horse", "horses", "pet", "pets", "vet", "kitten"]),
];

pub const REGISTER_MIN_HITS: usize = 4;

pub fn register(t: &str) -> &'static str {
    let toks = text::folded_tokens(t);
    let mut best = ("general_personal", REGISTER_MIN_HITS - 1);
    for (label, words) in REGISTERS {
        let n = toks.iter().filter(|w| words.contains(&w.as_str())).count();
        if n > best.1 {
            best = (label, n);
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(url: &str, host: &str, domain: &str) -> Option<(&'static str, String, bool)> {
        classify_url(url, host, domain).map(|b| (b.rule, b.blog_id, b.hosted))
    }
    #[test]
    fn blog_urls() {
        assert_eq!(c("http://anna.blogspot.com/2009/05/x.html", "anna.blogspot.com", "anna.blogspot.com"), Some(("blogspot", "anna.blogspot.com".into(), false)));
        assert_eq!(c("http://anna.blogspot.co.uk/", "anna.blogspot.co.uk", "anna.blogspot.co.uk").map(|x| x.0), Some("blogspot"));
        assert_eq!(c("https://cats.wordpress.com/2010/01/02/hello-world/", "cats.wordpress.com", "wordpress.com"), Some(("hosted_platform", "cats.wordpress.com".into(), true)));
        assert_eq!(c("https://en.wordpress.com/tag/cats/", "en.wordpress.com", "wordpress.com"), None);
        assert_eq!(c("https://medium.com/@someone/a-post-1234", "medium.com", "medium.com"), Some(("hosted_platform", "medium.com/@someone".into(), true)));
        assert_eq!(c("https://medium.com/some-pub/a-post-1234", "medium.com", "medium.com"), Some(("hosted_platform", "medium.com/some-pub".into(), true)));
        assert_eq!(c("https://medium.com/tag/x", "medium.com", "medium.com"), None);
        assert_eq!(c("http://users.livejournal.com/_joe_/12345.html", "users.livejournal.com", "livejournal.com"), Some(("hosted_platform", "users.livejournal.com/_joe_".into(), true)));
        assert_eq!(c("http://www.livejournal.com/users/joe/123.html", "www.livejournal.com", "livejournal.com"), Some(("hosted_platform", "livejournal.com/users/joe".into(), true)));
        assert_eq!(c("http://joe.livejournal.com/123.html", "joe.livejournal.com", "livejournal.com").map(|x| x.1), Some("joe.livejournal.com".into()));
        assert_eq!(c("http://www.xanga.com/joe/123/title", "www.xanga.com", "xanga.com").map(|x| x.1), Some("xanga.com/joe".into()));
        assert_eq!(c("https://blog.example.com/x", "blog.example.com", "example.com").map(|x| x.0), Some("blog_subdomain"));
        assert_eq!(c("https://example.com/blog/x", "example.com", "example.com").map(|x| x.0), Some("blog_path"));
        assert_eq!(c("https://www.example.com/2011/03/my-day.html", "www.example.com", "example.com").map(|x| x.0), Some("blogger_permalink"));
        assert_eq!(c("https://www.example.com/2011/03/14/my-long-day/", "www.example.com", "example.com").map(|x| x.0), Some("dated_slug_permalink"));
        assert_eq!(c("https://www.example.com/2011/13/14/my-long-day/", "www.example.com", "example.com"), None);
        assert_eq!(c("https://www.joesblog.net/x", "www.joesblog.net", "joesblog.net").map(|x| x.0), Some("blog_in_domain_name"));
        assert_eq!(c("https://www.example.com/products/widget", "www.example.com", "example.com"), None);
        let b = classify_url("https://cats.wordpress.com/x", "cats.wordpress.com", "wordpress.com").unwrap();
        assert_eq!(b.key("wordpress.com"), "fineweb_blog:cats.wordpress.com");
        let o = classify_url("https://example.com/blog/x", "example.com", "example.com").unwrap();
        assert_eq!(o.key("example.com"), "fineweb_site:example.com");
    }
    #[test]
    fn writer_identities_are_blogs_not_platforms() {
        let psl = crate::psl::Psl::parse("com\nuk\nco.uk\njp\norg\nblogspot.com\n");
        let w = |u: &str| writer_identity(&psl, u).unwrap();
        assert_eq!(w("https://cats.wordpress.com/2010/01/02/hello-world/"), ("fineweb_blog:cats.wordpress.com".into(), "hosted_blog"));
        assert_eq!(w("http://www.livejournal.com/users/joe/123.html"), ("fineweb_blog:livejournal.com/users/joe".into(), "hosted_blog"));
        assert_eq!(w("https://medium.com/@someone/a-post-1234"), ("fineweb_blog:medium.com/@someone".into(), "hosted_blog"));
        assert_eq!(w("https://anna.over-blog.com/article-1.html"), ("fineweb_blog:anna.over-blog.com".into(), "hosted_blog"));
        assert_eq!(w("https://en.wordpress.com/tag/cats/"), ("fineweb_site:wordpress.com".into(), "registered_domain"));
        assert_eq!(w("http://anna.blogspot.co.uk/2012/07/x.html"), ("fineweb_site:anna.blogspot.com".into(), "blogspot_country_domain"));
        assert_eq!(w("http://www.anna.blogspot.jp/2012/07/x.html"), ("fineweb_site:anna.blogspot.com".into(), "blogspot_country_domain"));
        assert_eq!(w("http://anna.blogspot.com/2012/07/x.html"), ("fineweb_site:anna.blogspot.com".into(), "registered_domain"));
        assert_eq!(w("http://www.blogspot.co.uk/"), ("fineweb_site:blogspot.co.uk".into(), "registered_domain"));
        assert_eq!(w("https://www.example.com/blog/x"), ("fineweb_site:example.com".into(), "registered_domain"));
    }
    #[test]
    fn first_person_and_register() {
        let fp = FirstPerson::of("I went to the market and my friend met me there. I bought bread for our dinner and we ate it.");
        assert_eq!((fp.singular, fp.plural), (4, 2));
        assert!(fp.passes(FIRST_PERSON_RULE));
        assert!(!FirstPerson::of("The committee reviewed the proposal and approved it after a short discussion of costs.").passes(FIRST_PERSON_RULE));
        assert_eq!(register("I baked a cake with flour, sugar and butter, then made soup in the kitchen for dinner."), "food_recipes");
        assert_eq!(register("I went outside and thought about it for a while."), "general_personal");
        assert!(blog_chrome("Anonymous said..."));
        assert!(blog_chrome("Labels: cats, life"));
        assert!(!blog_chrome("I said nothing at all to him that day."));
    }
}
