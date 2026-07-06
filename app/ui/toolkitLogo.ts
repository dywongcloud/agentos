// Maps a Composio toolkit slug to a simpleicons CDN logo URL (or null when we
// have no good icon). Shared by the main dashboard cards/tables and the agents
// workforce canvas chips.

const EXPLICIT: Record<string, string> = {
  gmail: "https://cdn.simpleicons.org/gmail",
  googlecalendar: "https://cdn.simpleicons.org/googlecalendar",
  google_calendar: "https://cdn.simpleicons.org/googlecalendar",
  googledrive: "https://cdn.simpleicons.org/googledrive",
  google_drive: "https://cdn.simpleicons.org/googledrive",
  googlecontacts: "https://cdn.simpleicons.org/googlecontacts",
  google_contacts: "https://cdn.simpleicons.org/googlecontacts",
  googlesheets: "https://cdn.simpleicons.org/googlesheets",
  google_sheets: "https://cdn.simpleicons.org/googlesheets",
  googledocs: "https://cdn.simpleicons.org/googledocs",
  google_docs: "https://cdn.simpleicons.org/googledocs",
  github: "https://cdn.simpleicons.org/github",
  gitlab: "https://cdn.simpleicons.org/gitlab",
  slack: "https://cdn.simpleicons.org/slack",
  notion: "https://cdn.simpleicons.org/notion",
  discord: "https://cdn.simpleicons.org/discord",
  linear: "https://cdn.simpleicons.org/linear",
  jira: "https://cdn.simpleicons.org/jira",
  atlassian: "https://cdn.simpleicons.org/atlassian",
  trello: "https://cdn.simpleicons.org/trello",
  asana: "https://cdn.simpleicons.org/asana",
  hubspot: "https://cdn.simpleicons.org/hubspot",
  salesforce: "https://cdn.simpleicons.org/salesforce",
  shopify: "https://cdn.simpleicons.org/shopify",
  stripe: "https://cdn.simpleicons.org/stripe",
  zoom: "https://cdn.simpleicons.org/zoom",
  dropbox: "https://cdn.simpleicons.org/dropbox",
  box: "https://cdn.simpleicons.org/box",
  airtable: "https://cdn.simpleicons.org/airtable",
  clickup: "https://cdn.simpleicons.org/clickup",
  figma: "https://cdn.simpleicons.org/figma",
  calendly: "https://cdn.simpleicons.org/calendly",
  resend: "https://cdn.simpleicons.org/resend",
  twilio: "https://cdn.simpleicons.org/twilio",
  whatsapp: "https://cdn.simpleicons.org/whatsapp",
  telegram: "https://cdn.simpleicons.org/telegram",
  zendesk: "https://cdn.simpleicons.org/zendesk",
  intercom: "https://cdn.simpleicons.org/intercom",
  postgres: "https://cdn.simpleicons.org/postgresql",
  postgresql: "https://cdn.simpleicons.org/postgresql",
  mysql: "https://cdn.simpleicons.org/mysql",
  mongodb: "https://cdn.simpleicons.org/mongodb",
  redis: "https://cdn.simpleicons.org/redis",
  vercel: "https://cdn.simpleicons.org/vercel",
  openai: "https://cdn.simpleicons.org/openai",
  anthropic: "https://cdn.simpleicons.org/anthropic",
  x: "https://cdn.simpleicons.org/x",
  twitter: "https://cdn.simpleicons.org/x",
  // LinkedIn was delisted from simpleicons (trademark) — use the stable
  // Wikimedia "in" mark so the chip renders instead of a broken image.
  linkedin: "https://upload.wikimedia.org/wikipedia/commons/c/ca/LinkedIn_logo_initials.png",
  reddit: "https://cdn.simpleicons.org/reddit",
  wechat: "https://cdn.simpleicons.org/wechat",
  weixin: "https://cdn.simpleicons.org/wechat",
  imessage: "https://cdn.simpleicons.org/imessage",
  exa: "https://cdn.simpleicons.org/googlesearchconsole",
  firecrawl: "https://cdn.simpleicons.org/firefoxbrowser",
  microsoftoutlook: "https://cdn.simpleicons.org/microsoftoutlook",
  microsoft_outlook: "https://cdn.simpleicons.org/microsoftoutlook",
  outlook: "https://cdn.simpleicons.org/microsoftoutlook",
  teams: "https://cdn.simpleicons.org/microsoftteams",
  microsoftteams: "https://cdn.simpleicons.org/microsoftteams",
  microsoft_teams: "https://cdn.simpleicons.org/microsoftteams",
  onedrive: "https://cdn.simpleicons.org/microsoftonedrive",
  microsoft_onedrive: "https://cdn.simpleicons.org/microsoftonedrive",
  sharepoint: "https://cdn.simpleicons.org/microsoftsharepoint",
  microsoft_sharepoint: "https://cdn.simpleicons.org/microsoftsharepoint",
};

export function toolkitLogo(slug: string): string | null {
  const key = slug.toLowerCase();
  if (EXPLICIT[key]) return EXPLICIT[key]!;
  const normalized = key.replace(/[^a-z0-9]/g, "");
  if (EXPLICIT[normalized]) return EXPLICIT[normalized]!;
  return null;
}

export function toolkitInitials(slug: string): string {
  const parts = slug.split(/[-_\s]+/).filter(Boolean);
  if (parts.length === 0) return "?";
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase();
  return `${parts[0]![0] ?? ""}${parts[1]![0] ?? ""}`.toUpperCase();
}
