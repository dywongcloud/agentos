"use client";

// app/ui/agents/AgentShowcase.tsx
//
// Per-agent showcase / detail view, opened from an "All agents" roster card.
// Modeled on the relay.app agent gallery card: title (agent name), a one-line
// blurb (persona), a two-column grid of capability cards derived from the
// agent's toolkits + skills, and a purple sprite panel showing the agent's
// generated pixel portrait. A "Browse agents" link closes back to the roster.

import type { CSSProperties } from "react";
import { toolkitLogo, toolkitInitials } from "@/app/ui/toolkitLogo";
import PixelAvatar from "@/app/ui/agents/PixelAvatar";

type ShowcaseAgent = {
  id: string;
  name: string;
  emoji: string;
  persona: string;
  toolkits: string[];
  skills: string[] | null;
};

// Prettify a toolkit slug for display: "google_drive" → "Google Drive".
function prettify(slug: string): string {
  return slug
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((p) => p.charAt(0).toUpperCase() + p.slice(1))
    .join(" ");
}

const cardStyle: CSSProperties = {
  background: "var(--card)",
  border: "1px solid var(--border)",
  borderRadius: 12,
  padding: 14,
  display: "flex",
  gap: 12,
  alignItems: "flex-start",
};

export default function AgentShowcase({
  agent,
  botUsername,
  modelName,
  onClose,
}: {
  agent: ShowcaseAgent;
  botUsername: string | null;
  modelName: string | null;
  onClose: () => void;
}) {
  const capabilities: Array<{ logo: string | null; title: string; sub: string }> = [
    ...agent.toolkits.map((tk) => ({
      logo: toolkitLogo(tk),
      title: prettify(tk),
      sub: "Connected toolkit",
    })),
    ...(agent.skills ?? []).map((sk) => ({
      logo: null,
      title: prettify(sk),
      sub: "Skill",
    })),
  ];

  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.45)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
        zIndex: 1000,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: "var(--background)",
          border: "1px solid var(--border)",
          borderRadius: 18,
          width: "min(840px, 100%)",
          maxHeight: "88vh",
          overflow: "auto",
          boxShadow: "0 24px 60px rgba(0,0,0,0.35)",
        }}
      >
        {/* header */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "14px 22px",
            borderBottom: "1px solid var(--border)",
          }}
        >
          <button
            onClick={onClose}
            style={{
              fontSize: 13,
              fontWeight: 600,
              color: "#6366f1",
              background: "transparent",
              border: "none",
              cursor: "pointer",
              padding: 0,
            }}
          >
            ← Browse agents
          </button>
          <button
            onClick={onClose}
            style={{
              fontSize: 18,
              color: "var(--muted-foreground)",
              background: "transparent",
              border: "none",
              cursor: "pointer",
              lineHeight: 1,
            }}
          >
            ×
          </button>
        </div>

        <div
          style={{
            display: "grid",
            gridTemplateColumns: "minmax(0,1fr) 280px",
            gap: 0,
          }}
        >
          {/* left: title, blurb, capabilities */}
          <div style={{ padding: 28 }}>
            <div
              style={{
                fontSize: 26,
                fontWeight: 800,
                color: "var(--foreground)",
                lineHeight: 1.2,
              }}
            >
              {agent.emoji} {agent.name}
            </div>
            <p
              style={{
                fontSize: 14.5,
                color: "var(--muted-foreground)",
                lineHeight: 1.6,
                margin: "12px 0 0",
              }}
            >
              {agent.persona || "A specialist sub-agent in this workforce."}
            </p>

            {(botUsername || modelName) && (
              <div style={{ display: "flex", gap: 8, marginTop: 14, flexWrap: "wrap" }}>
                {botUsername && (
                  <span
                    style={{
                      fontSize: 12,
                      fontWeight: 600,
                      color: "#6366f1",
                      background: "rgba(99,102,241,0.12)",
                      borderRadius: 99,
                      padding: "3px 10px",
                    }}
                  >
                    @{botUsername}
                  </span>
                )}
                {modelName && (
                  <span
                    style={{
                      fontSize: 12,
                      fontWeight: 600,
                      color: "var(--muted-foreground)",
                      background: "var(--muted)",
                      borderRadius: 99,
                      padding: "3px 10px",
                    }}
                  >
                    {modelName}
                  </span>
                )}
              </div>
            )}

            <div
              style={{
                fontSize: 12,
                fontWeight: 700,
                letterSpacing: 0.5,
                textTransform: "uppercase",
                color: "var(--muted-foreground)",
                margin: "26px 0 12px",
              }}
            >
              Capabilities
            </div>
            {capabilities.length === 0 ? (
              <p style={{ fontSize: 13, color: "var(--muted-foreground)" }}>
                No toolkits scoped yet — this agent reasons with its persona only.
              </p>
            ) : (
              <div
                style={{
                  display: "grid",
                  gridTemplateColumns: "1fr 1fr",
                  gap: 12,
                }}
              >
                {capabilities.map((c, i) => (
                  <div key={i} style={cardStyle}>
                    <div
                      style={{
                        width: 34,
                        height: 34,
                        borderRadius: 8,
                        background: "var(--muted)",
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        flexShrink: 0,
                        fontSize: 12,
                        fontWeight: 700,
                        color: "var(--muted-foreground)",
                        overflow: "hidden",
                      }}
                    >
                      {c.logo ? (
                        // eslint-disable-next-line @next/next/no-img-element
                        <img
                          src={c.logo}
                          alt=""
                          width={20}
                          height={20}
                          style={{ objectFit: "contain" }}
                        />
                      ) : (
                        toolkitInitials(c.title)
                      )}
                    </div>
                    <div style={{ minWidth: 0 }}>
                      <div
                        style={{
                          fontSize: 13.5,
                          fontWeight: 600,
                          color: "var(--foreground)",
                          whiteSpace: "nowrap",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                        }}
                      >
                        {c.title}
                      </div>
                      <div
                        style={{
                          fontSize: 11.5,
                          color: "var(--muted-foreground)",
                          marginTop: 2,
                        }}
                      >
                        {c.sub}
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* right: purple sprite panel */}
          <div
            style={{
              background: "linear-gradient(160deg, #6366f1 0%, #4338ca 100%)",
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              justifyContent: "center",
              gap: 16,
              padding: 28,
            }}
          >
            <div
              style={{
                background: "rgba(255,255,255,0.12)",
                border: "1px solid rgba(255,255,255,0.25)",
                borderRadius: 16,
                padding: 22,
                imageRendering: "pixelated",
              }}
            >
              <PixelAvatar seed={`${agent.id}:${agent.name}`} size={120} />
            </div>
            <div
              style={{
                fontSize: 13,
                fontWeight: 600,
                color: "rgba(255,255,255,0.92)",
                textAlign: "center",
              }}
            >
              {agent.toolkits.length} toolkit
              {agent.toolkits.length === 1 ? "" : "s"} ·{" "}
              {(agent.skills ?? []).length} skill
              {(agent.skills ?? []).length === 1 ? "" : "s"}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
