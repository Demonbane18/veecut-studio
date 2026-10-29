//! What a licence lets VeeCut do with a found file, and the credit line it asks for.
//!
//! Rule of thumb (from NextPhase Editor V1's sourcing notes, rewritten): anything unclear is
//! flagged and a person decides before the final render.

use std::sync::LazyLock;

use regex::Regex;

use super::{Candidate, MediaKind, Provider};

/// A licence in VeeCut's short form, with the reason a person must check it, if any.
#[derive(Debug, PartialEq)]
pub(crate) struct Rights {
    pub license: String,
    pub flag: Option<String>,
}

impl Rights {
    fn ok(license: impl Into<String>) -> Self {
        Rights {
            license: license.into(),
            flag: None,
        }
    }

    fn flagged(license: impl Into<String>, reason: impl Into<String>) -> Self {
        Rights {
            license: license.into(),
            flag: Some(reason.into()),
        }
    }
}

static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+\.\d+").unwrap());

/// Reads any licence text a provider gives (a short name, a template name, a licence URL or
/// several of them joined by spaces). Allowed without a check: public domain, the public domain
/// mark, CC0, CC BY, CC BY-SA and the Pexels License. Everything else is flagged: NC (no
/// commercial use), ND (VeeCut always re-colours, crops and cuts, which makes a derivative),
/// GFDL alone, fair use, and anything unknown or missing.
pub(crate) fn classify(text: &str) -> Rights {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() || lower == "unknown" {
        return Rights::flagged("unknown", "no licence found; check who owns it");
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &str| words.contains(&w);
    let any = |list: &[&str]| list.iter().any(|s| lower.contains(s));
    if any(&["fair use", "non-free"]) || has("nonfree") {
        return Rights::flagged(
            "fair use",
            "fair use is decided case by case, never by a licence; a person must decide",
        );
    }
    let cc_family = has("cc") || any(&["creativecommons", "creative commons"]);
    let version = VERSION.find(&lower).map(|m| format!(" {}", m.as_str()));
    let cc = |name: &str| format!("CC {name}{}", version.as_deref().unwrap_or(""));
    let nc = has("nc") || any(&["noncommercial", "non-commercial"]);
    let nd = has("nd") || any(&["noderiv", "no derivative", "no-derivative"]);
    let sa = has("sa") || any(&["sharealike", "share alike", "share-alike"]);
    if cc_family && (nc || nd) {
        let name = format!(
            "BY{}{}{}",
            if nc { "-NC" } else { "" },
            if sa { "-SA" } else { "" },
            if nd { "-ND" } else { "" }
        );
        let mut reasons = Vec::new();
        if nc {
            reasons.push("no commercial use (NC)");
        }
        if nd {
            reasons.push("no changes allowed (ND), and VeeCut always re-colours, crops and cuts");
        }
        return Rights::flagged(cc(&name), reasons.join("; "));
    }
    if has("cc0") || any(&["publicdomain/zero", "public domain dedication"]) {
        return Rights::ok("CC0");
    }
    if has("pdm") || has("pd") || any(&["publicdomain", "public domain", "public-domain"]) {
        return Rights::ok("public domain");
    }
    if cc_family && sa && (has("by") || lower.contains("attribution")) {
        return Rights::ok(cc("BY-SA"));
    }
    if cc_family && (has("by") || lower.contains("attribution")) {
        return Rights::ok(cc("BY"));
    }
    if lower.contains("pexels") {
        return Rights::ok("Pexels License");
    }
    if has("gfdl") || lower.contains("gnu free documentation") {
        return Rights::flagged(
            "GFDL",
            "GFDL only: it needs the whole licence text shipped with the video; check it or pick another file",
        );
    }
    Rights::flagged(
        text.trim(),
        format!(
            "VeeCut does not know the licence {:?}; check it",
            text.trim()
        ),
    )
}

/// A credit line ready for the video description.
pub(crate) fn credit(c: &Candidate) -> String {
    let author = c.author.as_deref().filter(|a| !a.trim().is_empty());
    if c.provider == Provider::Pexels {
        let what = match c.kind {
            MediaKind::Image => "Photo",
            MediaKind::Video => "Video",
        };
        return format!(
            "{what} by {} on Pexels: {}",
            author.unwrap_or("a Pexels creator"),
            c.page_url
        );
    }
    let by = author.map(|a| format!(" by {a}")).unwrap_or_default();
    let license = match &c.license_url {
        Some(url) if c.license.starts_with("CC") => format!("{} ({url})", c.license),
        _ => c.license.clone(),
    };
    // CC BY and BY-SA ask to say whether the work was changed; VeeCut always changes it.
    let changes = if c.license.starts_with("CC BY") {
        " Changes: re-coloured, cropped and cut."
    } else {
        ""
    };
    format!(
        "\"{}\"{by}, {license}, via {}: {}{changes}",
        c.title,
        c.provider.label(),
        c.page_url
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlisted_licences_are_not_flagged() {
        for (text, short) in [
            ("Public domain", "public domain"),
            ("pd", "public domain"),
            ("PD-USGov-NASA", "public domain"),
            (
                "http://creativecommons.org/licenses/publicdomain/",
                "public domain",
            ),
            (
                "https://creativecommons.org/publicdomain/mark/1.0/",
                "public domain",
            ),
            ("cc pdm 1.0", "public domain"),
            ("CC0", "CC0"),
            ("https://creativecommons.org/publicdomain/zero/1.0/", "CC0"),
            ("cc cc0 1.0", "CC0"),
            (
                "CC BY 4.0 https://creativecommons.org/licenses/by/4.0 cc-by-4.0",
                "CC BY 4.0",
            ),
            ("cc by 2.0", "CC BY 2.0"),
            ("CC BY-SA 3.0", "CC BY-SA 3.0"),
            (
                "https://creativecommons.org/licenses/by-sa/4.0/",
                "CC BY-SA 4.0",
            ),
            ("GFDL, cc-by-sa-3.0", "CC BY-SA 3.0"),
            ("Pexels License", "Pexels License"),
        ] {
            assert_eq!(classify(text), Rights::ok(short), "{text}");
        }
    }

    #[test]
    fn unclear_licences_are_flagged_with_a_reason() {
        for (text, short, reason) in [
            ("cc by-nc 2.0", "CC BY-NC 2.0", "no commercial use"),
            (
                "https://creativecommons.org/licenses/by-nc-sa/4.0/",
                "CC BY-NC-SA 4.0",
                "no commercial use",
            ),
            ("CC BY-ND 4.0", "CC BY-ND 4.0", "no changes allowed"),
            (
                "https://creativecommons.org/licenses/by-nc-nd/4.0/",
                "CC BY-NC-ND 4.0",
                "re-colours",
            ),
            ("GFDL", "GFDL", "GFDL only"),
            ("Fair use", "fair use", "case by case"),
            ("Some text nonfree", "fair use", "case by case"),
            ("", "unknown", "no licence"),
            ("unknown", "unknown", "no licence"),
            (
                "Copyrighted free use",
                "Copyrighted free use",
                "does not know",
            ),
            ("cc sampling+ 1.0", "cc sampling+ 1.0", "does not know"),
            ("Attribution", "Attribution", "does not know"),
        ] {
            let r = classify(text);
            assert_eq!(r.license, short, "{text}");
            let why = r.flag.unwrap_or_else(|| panic!("{text} should be flagged"));
            assert!(why.contains(reason), "{text}: {why}");
        }
    }

    fn candidate(
        provider: Provider,
        license: &str,
        url: Option<&str>,
        author: Option<&str>,
    ) -> Candidate {
        Candidate {
            id: "x:1".into(),
            provider,
            kind: MediaKind::Image,
            title: "Aurora and perseids".into(),
            page_url: "https://example.org/page".into(),
            thumb_url: String::new(),
            width: 0,
            height: 0,
            duration: 0.0,
            license: license.into(),
            license_url: url.map(Into::into),
            author: author.map(Into::into),
            flag: false,
            flag_reason: None,
        }
    }

    #[test]
    fn credit_lines_follow_the_licence() {
        let by = candidate(
            Provider::Commons,
            "CC BY 4.0",
            Some("https://creativecommons.org/licenses/by/4.0"),
            Some("Stephan Sprinz"),
        );
        assert_eq!(
            credit(&by),
            "\"Aurora and perseids\" by Stephan Sprinz, CC BY 4.0 (https://creativecommons.org/licenses/by/4.0), via Wikimedia Commons: https://example.org/page Changes: re-coloured, cropped and cut."
        );
        let pd = candidate(
            Provider::Nasa,
            "public domain",
            Some("https://www.nasa.gov/"),
            Some("NASA/Bill Ingalls"),
        );
        assert_eq!(
            credit(&pd),
            "\"Aurora and perseids\" by NASA/Bill Ingalls, public domain, via NASA: https://example.org/page"
        );
        let mut pexels = candidate(
            Provider::Pexels,
            "Pexels License",
            None,
            Some("Joey Farina"),
        );
        pexels.kind = MediaKind::Video;
        assert_eq!(
            credit(&pexels),
            "Video by Joey Farina on Pexels: https://example.org/page"
        );
        let anonymous = candidate(Provider::Archive, "public domain", None, None);
        assert_eq!(
            credit(&anonymous),
            "\"Aurora and perseids\", public domain, via Internet Archive: https://example.org/page"
        );
    }
}
