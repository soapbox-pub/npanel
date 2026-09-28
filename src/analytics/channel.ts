import { readFileSync } from "node:fs";
import { PAID_SOURCES } from "./source.ts";

/**
 * The marketing channel a visit belongs to: a port of Plausible's
 * `Ingestion.Acquisition`, which is Google Analytics 4's default channel
 * grouping with Plausible's adjustments.
 *
 * `source-categories.csv` is GA4's published list of which sources are search,
 * social, video, shopping or email, as Plausible ships it (8be0427). Plausible
 * layers categories of its own on top — AI assistants get a channel of their
 * own, and a few sources GA4 files nowhere are placed — and so does this.
 */

type Category = "SEARCH" | "SOCIAL" | "VIDEO" | "SHOPPING" | "EMAIL" | "AI_ASSISTANTS";

const CUSTOM_CATEGORIES: [string, Category][] = [
  ["hacker news", "SOCIAL"],
  ["yahoo!", "SEARCH"],
  ["gmail", "EMAIL"],
  ["telegram", "SOCIAL"],
  ["slack", "SOCIAL"],
  ["producthunt", "SOCIAL"],
  ["github", "SOCIAL"],
  ["steamcommunity.com", "SOCIAL"],
  ["statics.teams.cdn.office.net", "SOCIAL"],
  ["vkontakte", "SOCIAL"],
  ["threads", "SOCIAL"],
  ["ecosia", "SEARCH"],
  ["kagi", "SEARCH"],
  ["brave", "SEARCH"],
  ["temu.com", "SHOPPING"],
  ["discord", "SOCIAL"],
  ["sogou", "SEARCH"],
  ["microsoft teams", "SOCIAL"],
  ["bluesky", "SOCIAL"],
  ["mastodon", "SOCIAL"],
  ["chatgpt", "AI_ASSISTANTS"],
  ["claude", "AI_ASSISTANTS"],
  ["phind", "AI_ASSISTANTS"],
  ["deepseek", "AI_ASSISTANTS"],
  ["microsoft copilot", "AI_ASSISTANTS"],
  ["grok", "AI_ASSISTANTS"],
  ["google gemini", "AI_ASSISTANTS"],
  ["perplexity", "AI_ASSISTANTS"],
  ["x (twitter)", "SOCIAL"],
];

/**
 * Every categorized source, lowercased. GA4's entries win where both name a
 * source, since that is the order Plausible merges them in.
 */
const CATEGORIES = new Map<string, Category>(CUSTOM_CATEGORIES);
for (const line of readFileSync(new URL("./data/source-categories.csv", import.meta.url), "utf8").split("\n")) {
  const [source, category] = line.trim().split(",");
  if (!source || !category?.startsWith("SOURCE_CATEGORY_")) continue;
  CATEGORIES.set(source, category.slice("SOURCE_CATEGORY_".length) as Category);
}

const EMAIL_TAGS = ["email", "e-mail", "e_mail", "e mail", "newsletter"];

/** What a session was tagged with, as it arrived. */
export interface ChannelInputs {
  source: string;
  utmMedium: string;
  utmCampaign: string;
  utmSource: string;
  /** Which ad click id the landing URL carried, if any: `gclid`, `msclkid`, … */
  clickIdParam: string;
}

/** The channel for a session. The first rule that matches wins. */
export function channel(inputs: ChannelInputs): string {
  const source = inputs.source.toLowerCase();
  const medium = inputs.utmMedium.toLowerCase();
  const campaign = inputs.utmCampaign.toLowerCase();
  const utmSource = inputs.utmSource.toLowerCase();
  const category = CATEGORIES.get(source);

  const paidMedium = /^(.*cp.*|ppc|retargeting|paid.*)$/.test(medium);
  const paidSource = PAID_SOURCES.has(utmSource);
  const shoppingCampaign = /^(.*(([^a-df-z]|^)shop|shopping).*)$/.test(campaign);

  if (campaign.includes("cross-network")) return "Cross-network";
  if ((category === "SHOPPING" || shoppingCampaign) && paidMedium) return "Paid Shopping";
  if (
    (category === "SEARCH" && (paidMedium || paidSource)) ||
    (source === "google" && inputs.clickIdParam === "gclid") ||
    (source === "bing" && inputs.clickIdParam === "msclkid")
  ) {
    return "Paid Search";
  }
  if (category === "SOCIAL" && (paidMedium || paidSource)) return "Paid Social";
  if (category === "VIDEO" && (paidMedium || paidSource)) return "Paid Video";
  if (["display", "banner", "expandable", "interstitial", "cpm"].includes(medium)) return "Display";
  if (paidMedium) return "Paid Other";
  if (category === "SHOPPING" || shoppingCampaign) return "Organic Shopping";
  if (
    category === "SOCIAL" ||
    ["social", "social-network", "social-media", "sm", "social network", "social media"].includes(medium)
  ) {
    return "Organic Social";
  }
  if (category === "VIDEO" || medium.includes("video")) return "Organic Video";
  if (category === "AI_ASSISTANTS") return "AI Assistants";
  if (category === "SEARCH") return "Organic Search";
  if (
    category === "EMAIL" ||
    EMAIL_TAGS.some((tag) => utmSource.includes(tag)) ||
    EMAIL_TAGS.some((tag) => medium.includes(tag))
  ) {
    return "Email";
  }
  if (medium === "affiliate") return "Affiliates";
  if (medium === "audio") return "Audio";
  if (utmSource === "sms" || medium === "sms") return "SMS";
  if (
    medium.endsWith("push") ||
    medium.includes("mobile") ||
    medium.includes("notification") ||
    source === "firebase"
  ) {
    return "Mobile Push Notifications";
  }
  if (["referral", "app", "link"].includes(medium) || source !== "") return "Referral";
  return "Direct";
}
