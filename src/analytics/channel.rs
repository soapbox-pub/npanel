//! The marketing channel a visit belongs to: a port of Plausible's
//! `Ingestion.Acquisition`, which is Google Analytics 4's default channel
//! grouping with Plausible's adjustments.
//!
//! `source-categories.csv` is GA4's published list of which sources are
//! search, social, video, shopping or email, as Plausible ships it (8be0427).

use std::collections::HashMap;
use std::sync::OnceLock;

use super::source::is_paid_source;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Search,
    Social,
    Video,
    Shopping,
    Email,
    AiAssistants,
}

impl Category {
    fn parse(name: &str) -> Option<Category> {
        Some(match name {
            "SEARCH" => Category::Search,
            "SOCIAL" => Category::Social,
            "VIDEO" => Category::Video,
            "SHOPPING" => Category::Shopping,
            "EMAIL" => Category::Email,
            "AI_ASSISTANTS" => Category::AiAssistants,
            _ => return None,
        })
    }
}

const CUSTOM_CATEGORIES: [(&str, Category); 29] = [
    ("hacker news", Category::Social),
    ("yahoo!", Category::Search),
    ("gmail", Category::Email),
    ("telegram", Category::Social),
    ("slack", Category::Social),
    ("producthunt", Category::Social),
    ("github", Category::Social),
    ("steamcommunity.com", Category::Social),
    ("statics.teams.cdn.office.net", Category::Social),
    ("vkontakte", Category::Social),
    ("threads", Category::Social),
    ("ecosia", Category::Search),
    ("kagi", Category::Search),
    ("brave", Category::Search),
    ("temu.com", Category::Shopping),
    ("discord", Category::Social),
    ("sogou", Category::Search),
    ("microsoft teams", Category::Social),
    ("bluesky", Category::Social),
    ("mastodon", Category::Social),
    ("chatgpt", Category::AiAssistants),
    ("claude", Category::AiAssistants),
    ("phind", Category::AiAssistants),
    ("deepseek", Category::AiAssistants),
    ("microsoft copilot", Category::AiAssistants),
    ("grok", Category::AiAssistants),
    ("google gemini", Category::AiAssistants),
    ("perplexity", Category::AiAssistants),
    ("x (twitter)", Category::Social),
];

/// Every categorized source, lowercased. GA4's entries win where both name a
/// source, since that is the order Plausible merges them in.
fn categories() -> &'static HashMap<String, Category> {
    static CATEGORIES: OnceLock<HashMap<String, Category>> = OnceLock::new();
    CATEGORIES.get_or_init(|| {
        let mut map: HashMap<String, Category> = CUSTOM_CATEGORIES.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        for line in include_str!("../../assets/analytics/source-categories.csv").lines() {
            let mut parts = line.trim().split(',');
            let (Some(source), Some(category)) = (parts.next(), parts.next()) else { continue };
            let Some(category) = category.strip_prefix("SOURCE_CATEGORY_").and_then(Category::parse) else { continue };
            if !source.is_empty() {
                map.insert(source.to_string(), category);
            }
        }
        map
    })
}

const EMAIL_TAGS: [&str; 5] = ["email", "e-mail", "e_mail", "e mail", "newsletter"];

/// What a session was tagged with, as it arrived.
pub struct ChannelInputs<'a> {
    pub source: &'a str,
    pub utm_medium: &'a str,
    pub utm_campaign: &'a str,
    pub utm_source: &'a str,
    /// Which ad click id the landing URL carried, if any.
    pub click_id_param: &'a str,
}

/// `^(.*cp.*|ppc|retargeting|paid.*)$`
fn is_paid_medium(medium: &str) -> bool {
    medium.contains("cp") || medium == "ppc" || medium == "retargeting" || medium.starts_with("paid")
}

/// `^(.*(([^a-df-z]|^)shop|shopping).*)$`: `shop` at the start or after
/// anything but a letter other than `e`, or `shopping` anywhere.
fn is_shopping_campaign(campaign: &str) -> bool {
    if campaign.contains("shopping") {
        return true;
    }
    campaign.match_indices("shop").any(|(index, _)| {
        index == 0 || campaign[..index].chars().next_back().is_some_and(|c| !(('a'..='d').contains(&c) || ('f'..='z').contains(&c)))
    })
}

/// The channel for a session. The first rule that matches wins.
pub fn channel(inputs: ChannelInputs) -> &'static str {
    let source = inputs.source.to_lowercase();
    let medium = inputs.utm_medium.to_lowercase();
    let campaign = inputs.utm_campaign.to_lowercase();
    let utm_source = inputs.utm_source.to_lowercase();
    let category = categories().get(&source).copied();

    let paid_medium = is_paid_medium(&medium);
    let paid_source = is_paid_source(&utm_source);
    let shopping_campaign = is_shopping_campaign(&campaign);
    let is = |c: Category| category == Some(c);

    if campaign.contains("cross-network") {
        return "Cross-network";
    }
    if (is(Category::Shopping) || shopping_campaign) && paid_medium {
        return "Paid Shopping";
    }
    if (is(Category::Search) && (paid_medium || paid_source))
        || (source == "google" && inputs.click_id_param == "gclid")
        || (source == "bing" && inputs.click_id_param == "msclkid")
    {
        return "Paid Search";
    }
    if is(Category::Social) && (paid_medium || paid_source) {
        return "Paid Social";
    }
    if is(Category::Video) && (paid_medium || paid_source) {
        return "Paid Video";
    }
    if ["display", "banner", "expandable", "interstitial", "cpm"].contains(&medium.as_str()) {
        return "Display";
    }
    if paid_medium {
        return "Paid Other";
    }
    if is(Category::Shopping) || shopping_campaign {
        return "Organic Shopping";
    }
    if is(Category::Social) || ["social", "social-network", "social-media", "sm", "social network", "social media"].contains(&medium.as_str()) {
        return "Organic Social";
    }
    if is(Category::Video) || medium.contains("video") {
        return "Organic Video";
    }
    if is(Category::AiAssistants) {
        return "AI Assistants";
    }
    if is(Category::Search) {
        return "Organic Search";
    }
    if is(Category::Email) || EMAIL_TAGS.iter().any(|tag| utm_source.contains(tag) || medium.contains(tag)) {
        return "Email";
    }
    if medium == "affiliate" {
        return "Affiliates";
    }
    if medium == "audio" {
        return "Audio";
    }
    if utm_source == "sms" || medium == "sms" {
        return "SMS";
    }
    if medium.ends_with("push") || medium.contains("mobile") || medium.contains("notification") || source == "firebase" {
        return "Mobile Push Notifications";
    }
    if ["referral", "app", "link"].contains(&medium.as_str()) || !source.is_empty() {
        return "Referral";
    }
    "Direct"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(source: &str, medium: &str, campaign: &str, utm_source: &str, click: &str) -> &'static str {
        channel(ChannelInputs { source, utm_medium: medium, utm_campaign: campaign, utm_source, click_id_param: click })
    }

    #[test]
    fn channels() {
        assert_eq!(of("", "", "", "", ""), "Direct");
        assert_eq!(of("Google", "", "", "", ""), "Organic Search");
        assert_eq!(of("Google", "cpc", "", "", ""), "Paid Search");
        assert_eq!(of("Google", "", "", "", "gclid"), "Paid Search");
        assert_eq!(of("Facebook", "", "", "", ""), "Organic Social");
        assert_eq!(of("Facebook", "paid", "", "", ""), "Paid Social");
        assert_eq!(of("news.example.com", "", "", "", ""), "Referral");
        assert_eq!(of("", "email", "", "", ""), "Email");
        assert_eq!(of("", "", "summer-shop", "", ""), "Organic Shopping");
        assert_eq!(of("", "", "workshop", "", ""), "Direct");
        assert_eq!(of("ChatGPT", "", "", "", ""), "AI Assistants");
        assert_eq!(of("", "", "x-cross-network", "", ""), "Cross-network");
    }
}
