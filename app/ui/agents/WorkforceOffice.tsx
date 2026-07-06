"use client";

// WorkforceOffice — a focused "workplace" view for a single workforce.
//
// Where the canvas shows the flow graph, this shows the *team*: a stylized
// office room with a seated station per agent (deterministic PixelAvatar +
// nameplate + toolkit chips + memory count), shelves for shared knowledge and
// team memory, a shared-VFS cabinet, and a semantic memory search bar. Clicking
// a station opens a drawer with that agent's persona + private memories.
//
// All data comes from GET /api/ui/agents?op=office&teamId= (+ op=memory for the
// per-agent drawer, POST op=memory_search for the search bar). Purely additive;
// no server changes needed here.

import { useCallback, useEffect, useRef, useState } from "react";
import { toolkitLogo, toolkitInitials } from "@/app/ui/toolkitLogo";
import PixelAvatar from "@/app/ui/agents/PixelAvatar";

// Single indigo accent used for selected/working highlights — reads on both
// the light and dark theme surfaces (matches the canvas/tab accent).
const ACCENT = "#6366f1";

type OfficeAgent = {
  id: string;
  name: string;
  emoji: string;
  persona: string;
  toolkits: string[];
  stage: number;
  memoryCount: number;
  botBound: boolean;
};

type MemOut = {
  id: string;
  kind: string;
  scope: string;
  text: string;
  source: string | null;
  ts: number;
  score?: number;
};

type VfsFile = { path: string; updatedAt: string; size: number };

type OfficeData = {
  team: {
    id: string;
    name: string;
    emoji: string | null;
    spec: string;
    stages: number;
    enabled: boolean;
  };
  agents: OfficeAgent[];
  counts: { shared: number; workforce: number };
  workforceMemory: MemOut[];
  sharedMemory: MemOut[];
  files: VfsFile[];
};

export default function WorkforceOffice({
  userId,
  teamId,
  teamName,
  live,
  liveStageIndex,
}: {
  userId: string;
  teamId: string;
  teamName: string;
  live: boolean;
  liveStageIndex: number | null;
}) {
  const [data, setData] = useState<OfficeData | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [shelvesFlash, setShelvesFlash] = useState(false);

  // Clicking the in-room bookshelf scrolls the shared-knowledge / team-memory /
  // shared-files shelves into view and briefly highlights them.
  const shelvesRef = useRef<HTMLDivElement>(null);
  const openShelves = useCallback(() => {
    shelvesRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
    setShelvesFlash(true);
    setTimeout(() => setShelvesFlash(false), 1400);
  }, []);

  const load = useCallback(async () => {
    try {
      const res = await fetch(
        `/api/ui/agents?op=office&teamId=${encodeURIComponent(
          teamId
        )}&userId=${encodeURIComponent(userId)}`,
        { headers: { "content-type": "application/json" } }
      );
      const json = (await res.json().catch(() => null)) as
        | (OfficeData & { error?: string })
        | null;
      if (!res.ok || !json || json.error) {
        setErr(json?.error ?? `error ${res.status}`);
        return;
      }
      setData(json);
      setErr(null);
    } catch (e: any) {
      setErr(String(e?.message ?? e).slice(0, 160));
    }
  }, [teamId, userId]);

  useEffect(() => {
    setData(null);
    setSelectedAgentId(null);
    load();
  }, [load]);

  // Refresh periodically — faster while a run is live (memory/counts change),
  // slower otherwise (so newly created agents still arrive and the printer
  // animation can play without remounting the view).
  useEffect(() => {
    const t = setInterval(load, live ? 8000 : 12000);
    return () => clearInterval(t);
  }, [live, load]);

  const selectedAgent = data?.agents.find((a) => a.id === selectedAgentId) ?? null;

  if (err) {
    return (
      <div style={{ padding: 24, color: "var(--muted-foreground)" }}>
        Couldn’t load the office view: {err}
      </div>
    );
  }
  if (!data) {
    return (
      <div style={{ padding: 24, color: "var(--muted-foreground)" }}>
        Loading the workplace…
      </div>
    );
  }

  return (
    <div style={{ display: "flex", alignItems: "stretch", minHeight: 580 }}>
      {/* ---- the room ------------------------------------------------------ */}
      <div
        style={{
          flex: 1,
          minWidth: 0,
          position: "relative",
          borderRadius: 16,
          padding: "28px 24px 24px",
          overflow: "hidden",
          background: "var(--card)",
          border: "1px solid var(--border)",
        }}
      >
        {/* the room — a single isometric room for small teams, a multi-story
            flat office once the workforce gets big (5+ agents) */}
        {data.agents.length > 4 ? (
          <FlatRoom
            teamName={teamName || data.team.name}
            agents={data.agents}
            live={live}
            liveStageIndex={liveStageIndex}
            selectedAgentId={selectedAgentId}
            onSelect={setSelectedAgentId}
          />
        ) : (
          <IsoRoom
            teamName={teamName || data.team.name}
            agents={data.agents}
            live={live}
            liveStageIndex={liveStageIndex}
            selectedAgentId={selectedAgentId}
            onSelect={setSelectedAgentId}
            onOpenShelf={openShelves}
          />
        )}

        {/* shelves: shared knowledge + team memory + VFS cabinet */}
        <div
          ref={shelvesRef}
          style={{
            marginTop: 20,
            display: "grid",
            gridTemplateColumns: "1fr 1fr 1fr",
            gap: 12,
            borderRadius: 12,
            outline: shelvesFlash ? `2px solid ${ACCENT}` : "2px solid transparent",
            outlineOffset: 4,
            transition: "outline-color 0.4s",
          }}
        >
          <Shelf
            title="📚 Shared knowledge"
            subtitle={`${data.counts.shared} entries`}
            items={data.sharedMemory.map((m) => m.source ?? m.text)}
          />
          <Shelf
            title="🧠 Team memory"
            subtitle={`${data.counts.workforce} entries`}
            items={data.workforceMemory.map((m) => m.text)}
          />
          <Shelf
            title="🗄 Shared files"
            subtitle={`${data.files.length} file${data.files.length === 1 ? "" : "s"}`}
            items={data.files.map((f) => f.path)}
          />
        </div>

        {/* semantic memory search */}
        <MemorySearch userId={userId} teamId={teamId} />
      </div>

      {/* ---- agent drawer -------------------------------------------------- */}
      {selectedAgent && (
        <AgentDrawer
          userId={userId}
          agent={selectedAgent}
          onClose={() => setSelectedAgentId(null)}
        />
      )}
    </div>
  );
}

// --- isometric room: the AI workforce's home ---------------------------------
//
// The room itself is a single pixel-art backdrop sprite (public/office/
// building.png — walls, neon "HOME OF THE AI WORKFORCE" sign, windows and
// pendant lamps are baked in). On top of its empty isometric floor we overlay
// furniture sprites (sofa, bookshelf, coffee table + cup) and one seated
// "agent at a laptop" sprite per agent, depth-sorted and positioned via a
// calibrated affine projection of the floor diamond.

const BUILDING_W = 212; // building.png native pixel width — sprite widths are %s of this
const STAGE_AR = "212 / 228"; // building.png aspect ratio

// Map floor coords gx,gy ∈ [0,1] (gx → down-right/east edge, gy → down-left/west
// edge; 0,0 = back corner, 1,1 = front corner) to fractional position over the
// building image. Calibrated against building.png's painted floor diamond.
function floorPoint(gx: number, gy: number): [number, number] {
  return [0.5 + 0.443 * gx - 0.448 * gy, 0.557 + 0.206 * gx + 0.206 * gy];
}
function depthZ(gx: number, gy: number): number {
  return Math.round((gx + gy) * 100) + 10; // nearer (larger sum) overlaps farther
}

// Inverse of floorPoint: map a fractional point over the building image back to
// floor coords gx,gy. Used to turn a click into a walk destination.
function invFloorPoint(fx: number, fy: number): [number, number] {
  const a = 0.443, b = -0.448, c = 0.206, d = 0.206;
  const det = a * d - b * c;
  const X = fx - 0.5, Y = fy - 0.557;
  return [(d * X - b * Y) / det, (-c * X + a * Y) / det];
}
const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

// Pick a walk-sheet direction row from a grid-space movement vector.
// 0 = down-right (+gx), 1 = down-left (+gy), 2 = up-right (-gy), 3 = up-left (-gx).
function isoDir(dgx: number, dgy: number): number {
  if (Math.abs(dgx) >= Math.abs(dgy)) return dgx >= 0 ? 0 : 3;
  return dgy >= 0 ? 1 : 2;
}

// Asset cache-bust token: bump when a sprite-sheet PNG is rebuilt with new
// dimensions so browsers don't sample a stale sheet under the new CSS math
// (a cached old sitting.png stretched the seated couch into vertical bars).
const AV = "?v=5";

// ── character sprite sheet (public/office/workers.png) ──────────────────────
// 13 cols × 1 row of 100×150px cells — every agent uses the same character.
// Each frame is re-cropped so the character is horizontally centered and
// FEET-ALIGNED to the cell bottom — that keeps the body planted as the legs
// cycle, so the walk reads smooth instead of jittering frame to frame.
// Columns:
//   0 idle · 1–4 walk cycle · 5 idle · 6–8 working · 9–12 extras.
// All poses face LEFT, so we mirror (scaleX(-1)) when walking screen-rightward.
const WORKERS = {
  src: `/office/workers.png${AV}`,
  cols: 13,
  rows: 1,
  cellW: 100,
  cellH: 150,
  // Displayed width of the visible sub-rect as a % of the building image width.
  // Calibrated so the new (slimmer) character matches the old figure height.
  widthPct: 16.4,
  // Sample an inset sub-rect of each cell instead of the full cell: with
  // `image-rendering: pixelated` the browser's nearest-neighbour rounding at a
  // cell boundary can grab a pixel from the adjacent row (the neighbour's feet
  // flicker above a character's head). Insetting by a few sheet-pixels keeps
  // sampling strictly inside the cell's transparent guard bands.
  padX: 3,
  padY: 5,
};
// Couch-sitting sheet (public/office/sitting.png): 4 cols (gentle idle cycle)
// × 1 row — every agent uses the same seated-character art. Each frame contains
// the character already seated ON the furniture, so when an agent sits we swap
// the empty sofa.png frame for this sheet's cell instead of compositing.
const SITTING = {
  src: `/office/sitting.png${AV}`,
  cols: 4,
  rows: 1,
  cellW: 50,
  cellH: 50,
  // Matches the empty sofa Sprite exactly (frameW 50 × scale 0.95 / 212), so
  // the couch art doesn't resize when an agent sits down.
  widthPct: 22.4,
};
// The couch's floor coord and how close an agent must park to "sit" on it.
// When an idle agent rests within this radius we swap the empty sofa frame for
// the seated-person frames (1–4, animated) and hide its standing sprite.
const COUCH = { gx: 0.12, gy: 0.46 };
const COUCH_SIT_RADIUS = 0.16;

// ── agent printer (public/office/printer.png) ────────────────────────────────
// An isometric "agent printer" that fabricates new team members: when a fresh
// agent id appears after the office has mounted, the printer plays its build
// cycle (a crate materializing on the print bed), then the newborn agent steps
// out beside it and walks to a free spot. Idle, it shows the empty bed.
const PRINTER_SHEET = {
  src: `/office/printer.png${AV}`,
  frames: 16,
  frameW: 48,
  frameH: 59,
};
const PRINTER = { gx: 0.0, gy: 0.62 }; // front-left, against the left wall by the window
const PRINTER_OUT = { gx: 0.28, gy: 0.62 }; // where printed agents step out (into the room)
const PRINT_FRAME_MS = 150;

// ── ambient living-office decor (potted plant, desk workstation, robot vacuum) ─
// Static plant in the back-left corner; a desk in the front-right that idle
// agents occasionally stroll to and "work" at; and a robot vacuum that roams the
// floor on its own. Mirrors the reference animation's lived-in feel.
const PLANT = { gx: 0.52, gy: 0.07 };
const DESK = { gx: 0.72, gy: 0.58 };
const DESK_WORK = { gx: 0.6, gy: 0.58 }; // standing spot beside the desk to "work"

// ── multi-story flat office (public/office/building_tall.png) ────────────────
// For big workforces (5+ agents) the isometric single room gets cramped, so we
// switch to a straight-on cutaway of a taller building: several visible floors,
// agents distributed across them, each walking left↔right along its floor's
// baseline (no depth projection — purely 2-D). `floors` are the Y fractions of
// each floor's standing line (feet rest here), top → bottom; `xMin/xMax` bound
// the walkable strip inset from the walls.
// Calibrated against building_tall.png (1024×1536, 6 floors incl. ground).
const TALL = {
  src: `/office/building_tall.png${AV}`,
  ar: "1024 / 1536", // portrait cutaway aspect ratio
  floors: [0.16, 0.275, 0.385, 0.492, 0.611, 0.781], // feet-rest Y per floor
  xMin: 0.1,
  xMax: 0.9,
  widthPct: 15.1, // sprite width as % of building width (smaller: building is large)
};

// Single-character sheet: every agent renders the same variant (row 0). Kept
// as a function so a multi-variant sheet can be swapped back in later.
function variantRow(_id: string): number {
  return 0;
}

// One frame of a sprite-sheet PNG, bottom-center anchored at a floor point.
function Sprite({
  src,
  frames,
  frame,
  frameW,
  frameH,
  gx,
  gy,
  scale = 1,
  liftPct = 0,
  title,
  onClick,
  children,
}: {
  src: string;
  frames: number;
  frame: number;
  frameW: number;
  frameH: number;
  gx: number;
  gy: number;
  scale?: number;
  liftPct?: number; // shift up by this % of building height (e.g. to sit on a table)
  title?: string;
  onClick?: (e: React.MouseEvent) => void;
  children?: React.ReactNode;
}) {
  const [fx, fy] = floorPoint(gx, gy);
  const widthPct = ((frameW * scale) / BUILDING_W) * 100;
  return (
    <div
      onClick={onClick}
      title={title}
      style={{
        position: "absolute",
        left: `${fx * 100}%`,
        top: `${(fy - liftPct / 100) * 100}%`,
        width: `${widthPct}%`,
        transform: "translate(-50%, -100%)",
        zIndex: depthZ(gx, gy),
        cursor: onClick ? "pointer" : "default",
      }}
    >
      <FrameStrip src={src} frames={frames} frame={frame} frameW={frameW} frameH={frameH} />
      {children}
    </div>
  );
}

// The frame-windowing core of Sprite: a full-width box showing one frame of a
// horizontal strip. Position-agnostic so the flat office can reuse it.
function FrameStrip({
  src,
  frames,
  frame,
  frameW,
  frameH,
}: {
  src: string;
  frames: number;
  frame: number;
  frameW: number;
  frameH: number;
}) {
  return (
    <div
      style={{
        width: "100%",
        aspectRatio: `${frameW} / ${frameH}`,
        overflow: "hidden",
      }}
    >
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={src}
        alt=""
        draggable={false}
        style={{
          width: `${frames * 100}%`,
          height: "100%",
          marginLeft: `-${frame * 100}%`,
          imageRendering: "pixelated",
          display: "block",
        }}
      />
    </div>
  );
}

type Vec = { gx: number; gy: number };

function IsoRoom({
  teamName,
  agents,
  live,
  liveStageIndex,
  selectedAgentId,
  onSelect,
  onOpenShelf,
}: {
  teamName: string;
  agents: OfficeAgent[];
  live: boolean;
  liveStageIndex: number | null;
  selectedAgentId: string | null;
  onSelect: (id: string) => void;
  onOpenShelf?: () => void;
}) {
  // Animate decor (cup steam) only while a run is live.
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setTick((v) => v + 1), 220);
    return () => clearInterval(t);
  }, [live]);

  // Couch idle animation — always running so a seated agent gently shifts.
  // Ping-pong through the 4 poses (0,1,2,3,2,1,…) instead of wrapping 3→0, so
  // the loop never snaps across the full pose range (which read as choppy).
  const COUCH_SEQ = [0, 1, 2, 3, 2, 1];
  const [couchStep, setCouchStep] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setCouchStep((v) => (v + 1) % COUCH_SEQ.length), 520);
    return () => clearInterval(t);
  }, []);
  const couchFrame = COUCH_SEQ[couchStep];

  // Default standing spot for agent i — a tidy front-center grid, clear of the
  // back-corner decor. Agents start here and walk wherever you click.
  const seat = useCallback((i: number): Vec => {
    const col = i % 4;
    const row = Math.floor(i / 4);
    return { gx: 0.3 + col * 0.16, gy: 0.5 + row * 0.16 };
  }, []);

  // Live walk state lives in refs (mutated every animation frame); a frame
  // counter forces re-render only while something is actually moving.
  const stageRef = useRef<HTMLDivElement>(null);
  const posRef = useRef<Record<string, Vec>>({});
  const targetRef = useRef<Record<string, Vec | undefined>>({});
  const dirRef = useRef<Record<string, number>>({});
  const phaseRef = useRef<Record<string, number>>({});
  const [, forceRender] = useState(0);

  // Roaming robot vacuum: a single sprite that wanders the floor forever, picking
  // a fresh random target whenever it reaches the last one. Lives in refs and is
  // advanced inside the same animation frame as the agents.
  const vacPosRef = useRef<Vec>({ gx: 0.5, gy: 0.55 });
  const vacTargetRef = useRef<Vec>({ gx: 0.3, gy: 0.62 });
  const vacDirRef = useRef<number>(0);

  // Agent-printer queue: ids waiting to be fabricated, and the job in progress.
  const initializedRef = useRef(false);
  const printQueueRef = useRef<string[]>([]);
  const [printJob, setPrintJob] = useState<{ id: string; frame: number } | null>(
    null
  );
  // Mirror of printJob for use inside long-lived intervals (avoids stale capture).
  const printJobRef = useRef<{ id: string; frame: number } | null>(null);
  printJobRef.current = printJob;

  // Seed positions for any agent we haven't placed yet; drop stale ones.
  // Agents present on first render stand at their default seats; ids that show
  // up LATER are new hires — they queue at the printer instead.
  agents.forEach((a, i) => {
    if (!posRef.current[a.id]) {
      if (initializedRef.current) {
        posRef.current[a.id] = { ...PRINTER_OUT };
        if (!printQueueRef.current.includes(a.id)) printQueueRef.current.push(a.id);
      } else {
        posRef.current[a.id] = seat(i);
      }
    }
  });
  initializedRef.current = true;
  const ids = new Set(agents.map((a) => a.id));
  for (const id of Object.keys(posRef.current)) {
    if (!ids.has(id)) {
      delete posRef.current[id];
      delete targetRef.current[id];
    }
  }

  // Drive the printer: advance the build animation, then release the newborn
  // to walk to a free default seat; pull the next queued id when idle.
  useEffect(() => {
    const t = setInterval(() => {
      setPrintJob((p) => {
        if (p) {
          if (p.frame >= PRINTER_SHEET.frames - 1) {
            const idx = agents.findIndex((a) => a.id === p.id);
            if (idx >= 0) targetRef.current[p.id] = seat(idx);
            return null;
          }
          return { id: p.id, frame: p.frame + 1 };
        }
        const next = printQueueRef.current.shift();
        return next ? { id: next, frame: 0 } : null;
      });
    }, PRINT_FRAME_MS);
    return () => clearInterval(t);
  }, [agents, seat]);

  // Ambient life: while no run is live, periodically nudge an idle agent to
  // stroll somewhere — the couch to rest, the desk to "work," or a random floor
  // tile to mill about — so the office never feels frozen.
  useEffect(() => {
    if (live) return; // during a run agents move to their stage spots, not at random
    const t = setInterval(() => {
      const idle = agents.filter(
        (a) =>
          !targetRef.current[a.id] &&
          a.id !== printJobRef.current?.id &&
          !printQueueRef.current.includes(a.id)
      );
      if (!idle.length) return;
      const a = idle[Math.floor(Math.random() * idle.length)];
      const r = Math.random();
      let dest: Vec;
      if (r < 0.22) dest = { gx: COUCH.gx + 0.02, gy: COUCH.gy }; // rest on the couch
      else if (r < 0.42) dest = { ...DESK_WORK }; // go work at the desk
      else dest = { gx: 0.24 + Math.random() * 0.52, gy: 0.4 + Math.random() * 0.28 };
      targetRef.current[a.id] = dest;
      forceRender((v) => (v + 1) % 1e6);
    }, 3200);
    return () => clearInterval(t);
  }, [agents, live]);

  // Animation loop: step each walking agent toward its target.
  useEffect(() => {
    let raf = 0;
    let last = performance.now();
    const SPEED = 0.78; // grid units / second
    // Leg-cycle cadence is driven by DISTANCE walked, not wall-clock, so the
    // feet plant at ground speed (no "running in place" slide). One 4-frame
    // stride ≈ 0.18 grid units → phase advances 4 per 0.18 units ≈ ×22.
    const STRIDE_K = 22;
    const step = (now: number) => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      let moving = false;
      for (const id of Object.keys(targetRef.current)) {
        const t = targetRef.current[id];
        const p = posRef.current[id];
        if (!t || !p) continue;
        const dgx = t.gx - p.gx;
        const dgy = t.gy - p.gy;
        const dist = Math.hypot(dgx, dgy);
        if (dist < 0.01) {
          targetRef.current[id] = undefined;
          continue;
        }
        // Ease-out as the agent arrives so it settles instead of snapping.
        const speed = SPEED * (dist < 0.08 ? Math.max(0.35, dist / 0.08) : 1);
        const stepLen = Math.min(dist, speed * dt);
        p.gx += (dgx / dist) * stepLen;
        p.gy += (dgy / dist) * stepLen;
        dirRef.current[id] = isoDir(dgx, dgy);
        phaseRef.current[id] = (phaseRef.current[id] ?? 0) + stepLen * STRIDE_K;
        moving = true;
      }
      // Roaming vacuum — always creeping toward its target; on arrival it dwells
      // briefly then strikes out for a new random floor cell.
      {
        const vp = vacPosRef.current;
        const vt = vacTargetRef.current;
        const dgx = vt.gx - vp.gx;
        const dgy = vt.gy - vp.gy;
        const d = Math.hypot(dgx, dgy);
        if (d < 0.02) {
          vacTargetRef.current = {
            gx: 0.18 + Math.random() * 0.62,
            gy: 0.4 + Math.random() * 0.3,
          };
        } else {
          const sl = Math.min(d, 0.3 * dt); // slow, deliberate crawl
          vp.gx += (dgx / d) * sl;
          vp.gy += (dgy / d) * sl;
          vacDirRef.current = isoDir(dgx, dgy);
        }
        moving = true; // vacuum is always in motion → keep rendering
      }
      if (moving) forceRender((v) => (v + 1) % 1e6);
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  }, []);

  // Cursor → floor cell. Snap to a coarse grid so the hover marker and the walk
  // destination land on the same tile.
  const SNAP = 0.1;
  const cellFromEvent = useCallback((e: React.MouseEvent): Vec | null => {
    const el = stageRef.current;
    if (!el) return null;
    const rect = el.getBoundingClientRect();
    const fx = (e.clientX - rect.left) / rect.width;
    const fy = (e.clientY - rect.top) / rect.height;
    const [gx, gy] = invFloorPoint(fx, fy);
    return {
      gx: clamp(Math.round(gx / SNAP) * SNAP, 0, 1),
      gy: clamp(Math.round(gy / SNAP) * SNAP, 0, 1),
    };
  }, []);

  // The tile under the cursor — drawn as a white outlined diamond so you can see
  // where the selected agent will walk before you click.
  const [hoverCell, setHoverCell] = useState<Vec | null>(null);

  // Click on the floor → send the active agent walking to the hovered tile.
  const onFloorClick = useCallback(
    (e: React.MouseEvent) => {
      const cell = cellFromEvent(e);
      if (!cell) return;
      const id = selectedAgentId ?? agents[0]?.id;
      if (!id) return;
      targetRef.current[id] = cell;
      forceRender((v) => (v + 1) % 1e6);
    },
    [selectedAgentId, agents, cellFromEvent]
  );

  // Which agent (if any) is currently resting on the couch — the first idle
  // agent parked within the couch's radius. That agent renders as the seated
  // sofa sprite (with a floating nameplate) instead of a standing character.
  let sitterId: string | null = null;
  for (const a of agents) {
    const p = posRef.current[a.id];
    if (!p || targetRef.current[a.id]) continue; // unplaced or still walking
    if (Math.hypot(p.gx - COUCH.gx, p.gy - COUCH.gy) < COUCH_SIT_RADIUS) {
      sitterId = a.id;
      break;
    }
  }
  const sitter = sitterId ? agents.find((a) => a.id === sitterId) ?? null : null;
  const [couchFx, couchFy] = floorPoint(COUCH.gx, COUCH.gy);

  return (
    <div
      ref={stageRef}
      style={{
        position: "relative",
        width: "100%",
        maxWidth: 620,
        margin: "0 auto",
        aspectRatio: STAGE_AR,
      }}
    >
      {/* room backdrop */}
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={`/office/building.png${AV}`}
        alt="Home of the AI Workforce"
        draggable={false}
        style={{
          position: "absolute",
          inset: 0,
          width: "100%",
          height: "100%",
          imageRendering: "pixelated",
          display: "block",
        }}
      />

      {/* area rug — a flat floor decal, centered on the room's floor diamond.
          Sits above the painted floor but below the click-catcher + sprites. */}
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={`/office/rug.png${AV}`}
        alt=""
        draggable={false}
        style={{
          position: "absolute",
          left: `${(0.5 - 0.005 * 0.55) * 100}%`,
          top: `${(0.557 + 0.412 * 0.55) * 100}%`,
          width: `${1.782 * 0.38 * 100}%`,
          transform: "translate(-50%, -50%)",
          imageRendering: "pixelated",
          pointerEvents: "none",
          opacity: 0.92,
        }}
      />

      {/* floor click-catcher (below sprites; agents stopPropagation on click) */}
      <div
        onClick={onFloorClick}
        onMouseMove={(e) => agents.length && setHoverCell(cellFromEvent(e))}
        onMouseLeave={() => setHoverCell(null)}
        style={{
          position: "absolute",
          inset: 0,
          zIndex: 1,
          cursor: agents.length ? "pointer" : "default",
        }}
      />

      {/* hover marker: a white iso-tile outline on the floor cell under the
          cursor, showing where the selected agent will walk on click. */}
      {hoverCell && (
        <svg
          viewBox="0 0 100 100"
          preserveAspectRatio="none"
          style={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            zIndex: 2,
            pointerEvents: "none",
            overflow: "visible",
          }}
        >
          <polygon
            points={[
              [hoverCell.gx - SNAP / 2, hoverCell.gy - SNAP / 2],
              [hoverCell.gx + SNAP / 2, hoverCell.gy - SNAP / 2],
              [hoverCell.gx + SNAP / 2, hoverCell.gy + SNAP / 2],
              [hoverCell.gx - SNAP / 2, hoverCell.gy + SNAP / 2],
            ]
              .map(([gx, gy]) => {
                const [fx, fy] = floorPoint(gx, gy);
                return `${fx * 100},${fy * 100}`;
              })
              .join(" ")}
            fill="rgba(255,255,255,0.18)"
            stroke="rgba(255,255,255,0.95)"
            strokeWidth={0.5}
            strokeLinejoin="round"
          />
        </svg>
      )}

      {/* ---- furniture decor (back of the room) ---- */}
      {/* sofa: empty until an agent parks on it, then swap in that agent's own
          variant row from the sitting sheet (character + couch in one frame). */}
      {sitter ? (
        <SitSprite
          variant={variantRow(sitter.id)}
          frame={couchFrame}
          gx={COUCH.gx}
          gy={COUCH.gy}
          title={sitter.name}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(sitter.id);
          }}
        />
      ) : (
        <Sprite
          src={`/office/sofa.png${AV}`}
          frames={5}
          frame={0}
          frameW={50}
          frameH={50}
          gx={COUCH.gx}
          gy={COUCH.gy}
          scale={0.95}
        />
      )}
      <Sprite
        src={`/office/bookshelf.png${AV}`}
        frames={2}
        frame={0}
        frameW={40}
        frameH={52}
        gx={0.72}
        gy={0.1}
        scale={0.85}
        title="Shared knowledge"
        onClick={
          onOpenShelf
            ? (e) => {
                e.stopPropagation();
                onOpenShelf();
              }
            : undefined
        }
      />
      <Sprite src={`/office/table.png${AV}`} frames={2} frame={0} frameW={40} frameH={29} gx={0.3} gy={0.16} scale={0.85} />
      <Sprite src={`/office/cup.png${AV}`} frames={6} frame={live ? tick % 6 : 0} frameW={16} frameH={16} gx={0.3} gy={0.16} scale={0.7} liftPct={6} />

      {/* potted plant — against the back wall, between the table and bookshelf */}
      <Sprite src={`/office/plant.png${AV}`} frames={1} frame={0} frameW={34} frameH={46} gx={PLANT.gx} gy={PLANT.gy} scale={0.8} title="Office plant" />

      {/* desk workstation — front-right; idle agents wander here to "work" */}
      <Sprite src={`/office/desk.png${AV}`} frames={1} frame={0} frameW={56} frameH={46} gx={DESK.gx} gy={DESK.gy} scale={0.8} title="Workstation" />

      {/* roaming robot vacuum — autonomous floor patrol */}
      <Sprite
        src={`/office/vacuum.png${AV}`}
        frames={1}
        frame={0}
        frameW={30}
        frameH={20}
        gx={vacPosRef.current.gx}
        gy={vacPosRef.current.gy}
        scale={0.8}
        title="Robot vacuum"
      />

      {/* agent printer — runs its build cycle while a new agent fabricates */}
      <Sprite
        src={PRINTER_SHEET.src}
        frames={PRINTER_SHEET.frames}
        frame={printJob ? printJob.frame : 0}
        frameW={PRINTER_SHEET.frameW}
        frameH={PRINTER_SHEET.frameH}
        gx={PRINTER.gx}
        gy={PRINTER.gy}
        scale={0.7}
        title="Agent printer"
      >
        {printJob && (
          <div
            style={{
              position: "absolute",
              top: "-14%",
              left: "50%",
              transform: "translateX(-50%)",
              fontSize: 9,
              fontWeight: 700,
              color: "var(--foreground)",
              background: "var(--popover)",
              border: `1px solid ${ACCENT}`,
              boxShadow: `0 0 10px ${ACCENT}99`,
              borderRadius: 6,
              padding: "1px 6px",
              whiteSpace: "nowrap",
            }}
          >
            🖨 printing agent…
          </div>
        )}
      </Sprite>

      {/* nameplate that floats above the couch for the seated agent */}
      {sitter && (
        <div
          onClick={(e) => {
            e.stopPropagation();
            onSelect(sitter.id);
          }}
          style={{
            position: "absolute",
            left: `${couchFx * 100}%`,
            top: `${(couchFy - 0.135) * 100}%`,
            transform: "translate(-50%, -100%)",
            zIndex: depthZ(COUCH.gx, COUCH.gy) + 1,
            cursor: "pointer",
          }}
        >
          <NamePlate
            agent={sitter}
            working={live && liveStageIndex === sitter.stage}
            selected={sitter.id === selectedAgentId}
          />
        </div>
      )}

      {/* ---- agents (click the floor to walk a selected agent) ---- */}
      {agents.map((a) => {
        if (a.id === sitterId) return null; // rendered as the seated sofa sprite
        if (a.id === printJob?.id) return null; // still inside the printer
        if (printQueueRef.current.includes(a.id)) return null; // awaiting print
        const p = posRef.current[a.id]!;
        const working = live && liveStageIndex === a.stage;
        const selected = a.id === selectedAgentId;
        const isMoving = !!targetRef.current[a.id];
        return (
          <Character
            key={a.id}
            agent={a}
            gx={p.gx}
            gy={p.gy}
            dir={dirRef.current[a.id] ?? 0}
            phase={phaseRef.current[a.id] ?? 0}
            moving={isMoving}
            working={working}
            tick={tick}
            selected={selected}
            onSelect={() => onSelect(a.id)}
          />
        );
      })}

      {agents.length === 0 && (
        <div
          style={{
            position: "absolute",
            left: "50%",
            top: "62%",
            transform: "translate(-50%,-50%)",
            color: "var(--muted-foreground)",
            fontSize: 13,
            background: "var(--popover)",
            border: "1px solid var(--border)",
            padding: "8px 14px",
            borderRadius: 10,
            zIndex: 999,
          }}
        >
          No agents in {teamName} yet.
        </div>
      )}
    </div>
  );
}

// Flat front-elevation office for big workforces (5+ agents). A tall building
// backdrop with several floors; agents are spread round-robin across floors and
// walk left/right along each floor's baseline. Reuses CharacterSprite (with a
// smaller widthPct and the floor's Y baseline) so the sprites match the iso room
// exactly — only the coordinate resolution differs (1-D x instead of a diamond).
function FlatRoom({
  teamName,
  agents,
  live,
  liveStageIndex,
  selectedAgentId,
  onSelect,
}: {
  teamName: string;
  agents: OfficeAgent[];
  live: boolean;
  liveStageIndex: number | null;
  selectedAgentId: string | null;
  onSelect: (id: string) => void;
}) {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setTick((v) => v + 1), 220);
    return () => clearInterval(t);
  }, [live]);

  const floors = TALL.floors;

  // Default spot for agent i: round-robin onto floors, evenly spaced in x within
  // its floor so a crowded team doesn't pile up at one wall.
  const spot = useCallback(
    (i: number): { x: number; floor: number } => {
      const floor = i % floors.length;
      const lane = Math.floor(i / floors.length); // nth agent on this floor
      const perFloor = Math.ceil(agents.length / floors.length);
      const frac = perFloor <= 1 ? 0.5 : lane / (perFloor - 1);
      return { x: TALL.xMin + (TALL.xMax - TALL.xMin) * frac, floor };
    },
    [agents.length, floors.length]
  );

  const stageRef = useRef<HTMLDivElement>(null);
  const posRef = useRef<Record<string, { x: number; floor: number }>>({});
  const targetRef = useRef<Record<string, number | undefined>>({}); // target x
  const dirRef = useRef<Record<string, number>>({}); // +1 right, -1 left
  const phaseRef = useRef<Record<string, number>>({});
  const [, forceRender] = useState(0);

  // Agent-printer queue (ground-floor printer; same behavior as the iso room).
  const initializedRef = useRef(false);
  const printQueueRef = useRef<string[]>([]);
  const [printJob, setPrintJob] = useState<{ id: string; frame: number } | null>(
    null
  );
  const groundFloor = floors.length - 1;
  const PRINTER_X = 0.94;

  agents.forEach((a, i) => {
    if (!posRef.current[a.id]) {
      if (initializedRef.current) {
        posRef.current[a.id] = { x: PRINTER_X - 0.07, floor: groundFloor };
        if (!printQueueRef.current.includes(a.id)) printQueueRef.current.push(a.id);
      } else {
        posRef.current[a.id] = spot(i);
      }
    }
  });
  initializedRef.current = true;
  const ids = new Set(agents.map((a) => a.id));
  for (const id of Object.keys(posRef.current)) {
    if (!ids.has(id)) {
      delete posRef.current[id];
      delete targetRef.current[id];
    }
  }

  // Advance the print animation; on completion the newborn rides to its floor
  // and walks to its default spot.
  useEffect(() => {
    const t = setInterval(() => {
      setPrintJob((p) => {
        if (p) {
          if (p.frame >= PRINTER_SHEET.frames - 1) {
            const idx = agents.findIndex((a) => a.id === p.id);
            if (idx >= 0) {
              const dest = spot(idx);
              const pos = posRef.current[p.id];
              if (pos) pos.floor = dest.floor;
              targetRef.current[p.id] = dest.x;
            }
            return null;
          }
          return { id: p.id, frame: p.frame + 1 };
        }
        const next = printQueueRef.current.shift();
        return next ? { id: next, frame: 0 } : null;
      });
    }, PRINT_FRAME_MS);
    return () => clearInterval(t);
  }, [agents, spot]);

  // Animation loop: slide each walking agent toward its target x.
  useEffect(() => {
    let raf = 0;
    let last = performance.now();
    const SPEED = 0.3; // x-fraction / second
    const STRIDE_K = 80; // x is a 0–1 fraction, so leg cadence needs a bigger gain
    const step = (now: number) => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      let moving = false;
      for (const id of Object.keys(targetRef.current)) {
        const t = targetRef.current[id];
        const p = posRef.current[id];
        if (t == null || !p) continue;
        const dx = t - p.x;
        const dist = Math.abs(dx);
        if (dist < 0.004) {
          targetRef.current[id] = undefined;
          continue;
        }
        const speed = SPEED * (dist < 0.05 ? Math.max(0.35, dist / 0.05) : 1);
        const stepLen = Math.min(dist, speed * dt);
        p.x += Math.sign(dx) * stepLen;
        dirRef.current[id] = dx >= 0 ? 1 : -1;
        phaseRef.current[id] = (phaseRef.current[id] ?? 0) + stepLen * STRIDE_K;
        moving = true;
      }
      if (moving) forceRender((v) => (v + 1) % 1e6);
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  }, []);

  // Cursor → the spot (x on the nearest floor) the active agent would walk to.
  const spotFromEvent = useCallback(
    (e: React.MouseEvent): { x: number; floor: number } | null => {
      const el = stageRef.current;
      if (!el) return null;
      const rect = el.getBoundingClientRect();
      const fx = (e.clientX - rect.left) / rect.width;
      const fy = (e.clientY - rect.top) / rect.height;
      let best = 0;
      let bestD = Infinity;
      floors.forEach((y, idx) => {
        const d = Math.abs(y - fy);
        if (d < bestD) {
          bestD = d;
          best = idx;
        }
      });
      return { x: clamp(fx, TALL.xMin, TALL.xMax), floor: best };
    },
    [floors]
  );

  // White footprint box drawn on the floor under the cursor.
  const [hoverSpot, setHoverSpot] = useState<{ x: number; floor: number } | null>(null);

  // Click → walk the active agent to the hovered spot.
  const onFloorClick = useCallback(
    (e: React.MouseEvent) => {
      const spot = spotFromEvent(e);
      if (!spot) return;
      const id = selectedAgentId ?? agents[0]?.id;
      if (!id) return;
      const p = posRef.current[id];
      if (!p) return;
      p.floor = spot.floor;
      targetRef.current[id] = spot.x;
      forceRender((v) => (v + 1) % 1e6);
    },
    [selectedAgentId, agents, spotFromEvent]
  );

  return (
    <div
      ref={stageRef}
      style={{
        position: "relative",
        width: "100%",
        maxWidth: 640,
        margin: "0 auto",
        aspectRatio: TALL.ar,
      }}
    >
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={TALL.src}
        alt="Home of the AI Workforce"
        draggable={false}
        style={{
          position: "absolute",
          inset: 0,
          width: "100%",
          height: "100%",
          imageRendering: "pixelated",
          display: "block",
        }}
      />

      {/* floor click-catcher (below sprites) */}
      <div
        onClick={onFloorClick}
        onMouseMove={(e) => agents.length && setHoverSpot(spotFromEvent(e))}
        onMouseLeave={() => setHoverSpot(null)}
        style={{
          position: "absolute",
          inset: 0,
          zIndex: 1,
          cursor: agents.length ? "pointer" : "default",
        }}
      />

      {/* hover marker: a white footprint box on the floor under the cursor,
          showing where the selected agent will walk on click. */}
      {hoverSpot && (
        <div
          style={{
            position: "absolute",
            left: `${hoverSpot.x * 100}%`,
            top: `${floors[hoverSpot.floor] * 100}%`,
            width: `${TALL.widthPct}%`,
            aspectRatio: "2 / 1",
            transform: "translate(-50%, -85%)",
            border: "2px solid rgba(255,255,255,0.95)",
            background: "rgba(255,255,255,0.16)",
            borderRadius: 3,
            zIndex: 2,
            pointerEvents: "none",
          }}
        />
      )}

      {/* agent printer parked by the ground-floor wall */}
      <div
        title="Agent printer"
        style={{
          position: "absolute",
          left: `${PRINTER_X * 100}%`,
          top: `${floors[groundFloor] * 100}%`,
          width: `${TALL.widthPct * 0.8}%`,
          transform: "translate(-50%, -100%)",
          zIndex: groundFloor * 1000 + 5,
        }}
      >
        <FrameStrip
          src={PRINTER_SHEET.src}
          frames={PRINTER_SHEET.frames}
          frame={printJob ? printJob.frame : 0}
          frameW={PRINTER_SHEET.frameW}
          frameH={PRINTER_SHEET.frameH}
        />
        {printJob && (
          <div
            style={{
              position: "absolute",
              top: "-26%",
              left: "50%",
              transform: "translateX(-50%)",
              fontSize: 9,
              fontWeight: 700,
              color: "var(--foreground)",
              background: "var(--popover)",
              border: `1px solid ${ACCENT}`,
              boxShadow: `0 0 10px ${ACCENT}99`,
              borderRadius: 6,
              padding: "1px 6px",
              whiteSpace: "nowrap",
            }}
          >
            🖨 printing agent…
          </div>
        )}
      </div>

      {agents.map((a) => {
        if (a.id === printJob?.id) return null; // still inside the printer
        if (printQueueRef.current.includes(a.id)) return null; // awaiting print
        const p = posRef.current[a.id]!;
        const working = live && liveStageIndex === a.stage;
        const selected = a.id === selectedAgentId;
        const isMoving = targetRef.current[a.id] != null;
        const row = variantRow(a.id);
        const dir = dirRef.current[a.id] ?? -1;
        const phase = phaseRef.current[a.id] ?? 0;
        const facingRight = dir > 0;
        const col = isMoving
          ? 1 + (Math.floor(phase) % 4)
          : working
            ? 6 + (tick % 3)
            : 0;
        const bob = isMoving ? Math.abs(Math.sin(phase * Math.PI)) * 0.4 : 0;
        const fy = floors[p.floor];
        // Lower floors (larger index, nearer the viewer) overlap upper ones;
        // tie-break by x so same-floor neighbours layer left-to-right.
        const z = p.floor * 1000 + Math.round(p.x * 100) + 10;
        return (
          <CharacterSprite
            key={a.id}
            row={row}
            col={col}
            flip={facingRight}
            fx={p.x}
            fy={fy}
            z={z}
            liftPct={bob}
            widthPct={TALL.widthPct}
            title={a.name}
            onClick={(e) => {
              e.stopPropagation();
              onSelect(a.id);
            }}
          >
            <AgentTag agent={a} working={working} selected={selected} />
          </CharacterSprite>
        );
      })}

      {agents.length === 0 && (
        <div
          style={{
            position: "absolute",
            left: "50%",
            top: "50%",
            transform: "translate(-50%,-50%)",
            color: "var(--muted-foreground)",
            fontSize: 13,
            background: "var(--popover)",
            border: "1px solid var(--border)",
            padding: "8px 14px",
            borderRadius: 10,
            zIndex: 999,
          }}
        >
          No agents in {teamName} yet.
        </div>
      )}
    </div>
  );
}

// A single agent character drawn from the workers.png grid. Bottom-center
// anchored at its floor point, with a walk bob + nameplate.
//   moving  → walk cycle (cols 1–4)
//   working → sitting at a desk (cols 6–8), animated by `tick`
//   idle    → standing (col 0)
function Character({
  agent,
  gx,
  gy,
  dir,
  phase,
  moving,
  working,
  tick,
  selected,
  onSelect,
}: {
  agent: OfficeAgent;
  gx: number;
  gy: number;
  dir: number;
  phase: number;
  moving: boolean;
  working: boolean;
  tick: number;
  selected: boolean;
  onSelect: () => void;
}) {
  const handleClick = (e: React.MouseEvent) => {
    e.stopPropagation(); // don't also trigger a floor walk
    onSelect();
  };
  const row = variantRow(agent.id);
  const facingRight = dir === 0 || dir === 2; // moving toward screen-right
  const col = moving
    ? 1 + (Math.floor(phase) % 4)
    : working
      ? 6 + (tick % 3)
      : 0;
  const bob = moving ? Math.abs(Math.sin(phase * Math.PI)) * 0.6 : 0;

  const [fx, fy] = floorPoint(gx, gy);
  return (
    <CharacterSprite
      row={row}
      col={col}
      flip={facingRight}
      fx={fx}
      fy={fy}
      z={depthZ(gx, gy)}
      liftPct={bob}
      title={agent.name}
      onClick={handleClick}
    >
      <AgentTag agent={agent} working={working} selected={selected} />
    </CharacterSprite>
  );
}

// Renders one cell of the workers grid via background-position, anchored
// bottom-center at an already-resolved screen point (fx,fy fractions); mirrors
// horizontally when `flip`. Used by both the isometric room and the flat
// multi-story office — they differ only in how they resolve fx/fy/z.
function CharacterSprite({
  row,
  col,
  flip,
  fx,
  fy,
  z,
  liftPct = 0,
  widthPct = WORKERS.widthPct,
  title,
  onClick,
  children,
}: {
  row: number;
  col: number;
  flip: boolean;
  fx: number;
  fy: number;
  z: number;
  liftPct?: number;
  widthPct?: number;
  title?: string;
  onClick?: (e: React.MouseEvent) => void;
  children?: React.ReactNode;
}) {
  const { cols: C, rows: R, cellW, cellH, padX, padY } = WORKERS;
  // Visible sub-rect of the cell (inset to dodge neighbour-cell bleed); map it
  // to the full element. background-position % is offset/(bgSize − element),
  // background-size % is sheetDim/visibleDim.
  const visW = cellW - 2 * padX;
  const visH = cellH - 2 * padY;
  const posX = ((col * cellW + padX) / (C * cellW - visW)) * 100;
  const posY = ((row * cellH + padY) / (R * cellH - visH)) * 100;
  return (
    <div
      onClick={onClick}
      title={title}
      style={{
        position: "absolute",
        left: `${fx * 100}%`,
        top: `${(fy - liftPct / 100) * 100}%`,
        width: `${widthPct}%`,
        aspectRatio: `${visW} / ${visH}`,
        transform: "translate(-50%, -100%)",
        zIndex: z,
        cursor: onClick ? "pointer" : "default",
      }}
    >
      <div
        style={{
          width: "100%",
          height: "100%",
          backgroundImage: `url(${WORKERS.src})`,
          backgroundRepeat: "no-repeat",
          backgroundSize: `${((C * cellW) / visW) * 100}% ${((R * cellH) / visH) * 100}%`,
          backgroundPosition: `${posX}% ${posY}%`,
          imageRendering: "pixelated",
          transform: flip ? "scaleX(-1)" : undefined,
        }}
      />
      {children}
    </div>
  );
}

// One cell of the per-variant sitting sheet (character seated on the couch),
// bottom-center anchored at the couch's floor point like any other sprite.
function SitSprite({
  variant,
  frame,
  gx,
  gy,
  title,
  onClick,
}: {
  variant: number;
  frame: number;
  gx: number;
  gy: number;
  title?: string;
  onClick?: (e: React.MouseEvent) => void;
}) {
  const { cols: C, rows: R } = SITTING;
  const [fx, fy] = floorPoint(gx, gy);
  const row = variant % R;
  const col = frame % C;
  return (
    <div
      onClick={onClick}
      title={title}
      style={{
        position: "absolute",
        left: `${fx * 100}%`,
        top: `${fy * 100}%`,
        width: `${SITTING.widthPct}%`,
        aspectRatio: `${SITTING.cellW} / ${SITTING.cellH}`,
        transform: "translate(-50%, -100%)",
        zIndex: depthZ(gx, gy),
        cursor: onClick ? "pointer" : "default",
      }}
    >
      <div
        style={{
          width: "100%",
          height: "100%",
          backgroundImage: `url(${SITTING.src})`,
          backgroundRepeat: "no-repeat",
          backgroundSize: `${C * 100}% ${R * 100}%`,
          backgroundPosition: `${C > 1 ? (col / (C - 1)) * 100 : 0}% ${R > 1 ? (row / (R - 1)) * 100 : 0}%`,
          imageRendering: "pixelated",
        }}
      />
    </div>
  );
}

// Nameplate + status pip that hangs just below a seated agent sprite.
function AgentTag({
  agent,
  working,
  selected,
}: {
  agent: OfficeAgent;
  working: boolean;
  selected: boolean;
}) {
  return (
    <div
      style={{
        position: "absolute",
        top: "100%",
        left: "50%",
        transform: "translateX(-50%)",
        marginTop: 3,
        pointerEvents: "none",
      }}
    >
      <NamePlate agent={agent} working={working} selected={selected} />
    </div>
  );
}

// The badge + status pip itself (theme-driven so it reads on light and dark).
// Used both below a standing character and floating above the couch.
function NamePlate({
  agent,
  working,
  selected,
}: {
  agent: OfficeAgent;
  working: boolean;
  selected: boolean;
}) {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        gap: 2,
      }}
    >
      <div
        style={{
          fontSize: 9.5,
          fontWeight: 700,
          color: "var(--foreground)",
          background: "var(--popover)",
          border: `1px solid ${selected || working ? ACCENT : "var(--border)"}`,
          boxShadow: working ? `0 0 10px ${ACCENT}99` : "none",
          borderRadius: 6,
          padding: "1px 6px",
          whiteSpace: "nowrap",
          maxWidth: 120,
          overflow: "hidden",
          textOverflow: "ellipsis",
        }}
      >
        {agent.emoji} {agent.name}
      </div>
      {(working || agent.botBound) && (
        <div
          style={{
            fontSize: 8.5,
            color: working ? ACCENT : "var(--muted-foreground)",
          }}
        >
          {working ? "● working" : "🤖 bot"}
        </div>
      )}
    </div>
  );
}

function MiniChips({ toolkits }: { toolkits: string[] }) {
  return (
    <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
      {toolkits.slice(0, 6).map((slug) => {
        const logo = toolkitLogo(slug);
        return (
          <span
            key={slug}
            title={slug}
            style={{
              width: 16,
              height: 16,
              borderRadius: 4,
              background: "var(--muted)",
              border: "1px solid var(--border)",
              display: "inline-flex",
              alignItems: "center",
              justifyContent: "center",
            }}
          >
            {logo ? (
              // eslint-disable-next-line @next/next/no-img-element
              <img src={logo} alt={slug} style={{ width: 10, height: 10 }} />
            ) : (
              <span style={{ fontSize: 6, fontWeight: 700, color: "var(--muted-foreground)" }}>
                {toolkitInitials(slug)}
              </span>
            )}
          </span>
        );
      })}
    </div>
  );
}

// --- shelf (shared knowledge / team memory / files) --------------------------

function Shelf({
  title,
  subtitle,
  items,
}: {
  title: string;
  subtitle: string;
  items: string[];
}) {
  return (
    <div
      style={{
        borderRadius: 12,
        padding: 12,
        background: "var(--muted)",
        border: "1px solid var(--border)",
        minHeight: 110,
      }}
    >
      <div style={{ fontSize: 12, fontWeight: 700, color: "var(--foreground)" }}>{title}</div>
      <div style={{ fontSize: 10, color: "var(--muted-foreground)", marginBottom: 8 }}>
        {subtitle}
      </div>
      {items.length === 0 ? (
        <div style={{ fontSize: 11, color: "var(--muted-foreground)", fontStyle: "italic" }}>
          empty
        </div>
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {items.slice(0, 5).map((t, i) => (
            <div
              key={i}
              style={{
                fontSize: 11,
                color: "var(--foreground)",
                whiteSpace: "nowrap",
                overflow: "hidden",
                textOverflow: "ellipsis",
                paddingLeft: 8,
                borderLeft: `2px solid ${ACCENT}`,
              }}
            >
              {t}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// --- semantic memory search bar ----------------------------------------------

function MemorySearch({ userId, teamId }: { userId: string; teamId: string }) {
  const [q, setQ] = useState("");
  const [busy, setBusy] = useState(false);
  const [hits, setHits] = useState<MemOut[] | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const run = useCallback(async () => {
    const query = q.trim();
    if (!query || busy) return;
    setBusy(true);
    setErr(null);
    try {
      const res = await fetch(
        `/api/ui/agents?userId=${encodeURIComponent(userId)}`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            op: "memory_search",
            query,
            workforceId: teamId,
          }),
        }
      );
      const json = (await res.json().catch(() => null)) as
        | { hits?: MemOut[]; error?: string }
        | null;
      if (!res.ok || !json || json.error) {
        setErr(json?.error ?? `error ${res.status}`);
        return;
      }
      setHits(json.hits ?? []);
    } catch (e: any) {
      setErr(String(e?.message ?? e).slice(0, 160));
    } finally {
      setBusy(false);
    }
  }, [q, busy, userId, teamId]);

  return (
    <div style={{ marginTop: 16 }}>
      <div style={{ display: "flex", gap: 8 }}>
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") run();
          }}
          placeholder="Search team & shared memory…"
          style={{
            flex: 1,
            background: "var(--muted)",
            border: "1px solid var(--border)",
            borderRadius: 10,
            padding: "8px 12px",
            color: "var(--foreground)",
            fontSize: 13,
            outline: "none",
          }}
        />
        <button
          onClick={run}
          disabled={busy || !q.trim()}
          style={{
            background: ACCENT,
            border: "none",
            borderRadius: 10,
            padding: "8px 16px",
            color: "#fff",
            fontSize: 13,
            fontWeight: 600,
            cursor: busy || !q.trim() ? "default" : "pointer",
            opacity: busy || !q.trim() ? 0.5 : 1,
          }}
        >
          {busy ? "…" : "Search"}
        </button>
      </div>
      {err && (
        <div style={{ marginTop: 8, fontSize: 11, color: "var(--destructive)" }}>{err}</div>
      )}
      {hits && (
        <div style={{ marginTop: 10, display: "flex", flexDirection: "column", gap: 6 }}>
          {hits.length === 0 ? (
            <div style={{ fontSize: 12, color: "var(--muted-foreground)" }}>No matches.</div>
          ) : (
            hits.map((h) => (
              <div
                key={h.id}
                style={{
                  fontSize: 12,
                  color: "var(--foreground)",
                  background: "var(--muted)",
                  border: "1px solid var(--border)",
                  borderRadius: 8,
                  padding: "6px 10px",
                }}
              >
                <span
                  style={{
                    fontSize: 9,
                    fontWeight: 700,
                    color: "var(--muted-foreground)",
                    marginRight: 6,
                    textTransform: "uppercase",
                  }}
                >
                  {h.scope}
                  {h.score != null ? ` · ${h.score}` : ""}
                </span>
                {h.text}
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}

// --- per-agent drawer (persona + private memory) -----------------------------

function AgentDrawer({
  userId,
  agent,
  onClose,
}: {
  userId: string;
  agent: OfficeAgent;
  onClose: () => void;
}) {
  const [mems, setMems] = useState<MemOut[] | null>(null);

  useEffect(() => {
    let alive = true;
    setMems(null);
    fetch(
      `/api/ui/agents?op=memory&scope=agent&agentId=${encodeURIComponent(
        agent.id
      )}&userId=${encodeURIComponent(userId)}`,
      { headers: { "content-type": "application/json" } }
    )
      .then((r) => r.json().catch(() => null))
      .then((j: { memories?: MemOut[] } | null) => {
        if (alive) setMems(j?.memories ?? []);
      })
      .catch(() => alive && setMems([]));
    return () => {
      alive = false;
    };
  }, [agent.id, userId]);

  return (
    <div
      style={{
        width: 320,
        flexShrink: 0,
        marginLeft: 12,
        borderRadius: 16,
        background: "var(--card)",
        border: "1px solid var(--border)",
        padding: 16,
        overflowY: "auto",
        maxHeight: 580,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <div
          style={{
            borderRadius: 12,
            overflow: "hidden",
            border: "1px solid var(--border)",
          }}
        >
          <PixelAvatar seed={`${agent.id}:${agent.name}`} size={48} />
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontSize: 15, fontWeight: 700, color: "var(--foreground)" }}>
            {agent.emoji} {agent.name}
          </div>
          <div style={{ fontSize: 11, color: "var(--muted-foreground)" }}>
            🧠 {agent.memoryCount} memories
            {agent.botBound ? " · 🤖 bot bound" : ""}
          </div>
        </div>
        <button
          onClick={onClose}
          style={{
            background: "transparent",
            border: "none",
            color: "var(--muted-foreground)",
            fontSize: 18,
            cursor: "pointer",
          }}
        >
          ×
        </button>
      </div>

      <div style={{ marginTop: 12 }}>
        <MiniChips toolkits={agent.toolkits} />
      </div>

      <div
        style={{
          marginTop: 14,
          fontSize: 12,
          lineHeight: 1.5,
          color: "var(--foreground)",
          whiteSpace: "pre-wrap",
        }}
      >
        {agent.persona || "No persona set."}
      </div>

      <div
        style={{
          marginTop: 16,
          fontSize: 11,
          fontWeight: 700,
          letterSpacing: 1,
          textTransform: "uppercase",
          color: "var(--muted-foreground)",
        }}
      >
        Private memory
      </div>
      {mems === null ? (
        <div style={{ marginTop: 8, fontSize: 12, color: "var(--muted-foreground)" }}>
          Loading…
        </div>
      ) : mems.length === 0 ? (
        <div style={{ marginTop: 8, fontSize: 12, color: "var(--muted-foreground)", fontStyle: "italic" }}>
          No private memories yet.
        </div>
      ) : (
        <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 6 }}>
          {mems.map((m) => (
            <div
              key={m.id}
              style={{
                fontSize: 12,
                color: "var(--foreground)",
                background: "var(--muted)",
                border: "1px solid var(--border)",
                borderRadius: 8,
                padding: "6px 10px",
              }}
            >
              <span
                style={{
                  fontSize: 9,
                  fontWeight: 700,
                  color: "var(--muted-foreground)",
                  marginRight: 6,
                  textTransform: "uppercase",
                }}
              >
                {m.kind}
              </span>
              {m.text}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
