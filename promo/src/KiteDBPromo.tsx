import {
  AbsoluteFill,
  interpolate,
  Sequence,
  spring,
  useCurrentFrame,
  useVideoConfig,
  Easing,
} from "remotion";
import { KiteLogo } from "./KiteLogo";
import { theme } from "./theme";

// ============================================================================
// SHARED COMPONENTS
// ============================================================================

// Background with grid and glow - persistent across all scenes
const Background: React.FC = () => {
  const frame = useCurrentFrame();
  const gridOffset = frame * 0.3;

  return (
    <AbsoluteFill
      style={{
        background: `
          radial-gradient(800px 400px at 30% 30%, rgba(42, 242, 255, 0.08), transparent 60%),
          radial-gradient(600px 380px at 70% 60%, rgba(56, 247, 201, 0.06), transparent 65%),
          linear-gradient(120deg, #05070d 0%, #0a1018 40%, #05070d 100%)
        `,
      }}
    >
      {/* Animated Grid */}
      <div
        style={{
          position: "absolute",
          inset: 0,
          backgroundImage: `
            linear-gradient(rgba(42, 242, 255, 0.05) 1px, transparent 1px),
            linear-gradient(90deg, rgba(42, 242, 255, 0.05) 1px, transparent 1px)
          `,
          backgroundSize: "60px 60px",
          backgroundPosition: `0 ${gridOffset}px`,
          maskImage:
            "radial-gradient(circle at 50% 50%, black 30%, transparent 70%)",
          opacity: 0.6,
        }}
      />

      {/* Speed lines */}
      <div
        style={{
          position: "absolute",
          inset: "-20%",
          background: `repeating-linear-gradient(
            110deg,
            rgba(42, 242, 255, 0.03) 0px,
            rgba(42, 242, 255, 0.03) 2px,
            transparent 2px,
            transparent 30px
          )`,
          transform: `translateX(${((frame * 2) % 100) - 50}%) skewX(-12deg)`,
          opacity: 0.4,
        }}
      />
    </AbsoluteFill>
  );
};

// Blinking cursor component
const Cursor: React.FC<{ frame: number; visible?: boolean }> = ({
  frame,
  visible = true,
}) => {
  if (!visible) return null;
  const opacity = Math.sin(frame * 0.2) > 0 ? 1 : 0;
  return (
    <span
      style={{
        display: "inline-block",
        width: 12,
        height: 26,
        background: theme.accent,
        marginLeft: 2,
        opacity,
        boxShadow: `0 0 8px ${theme.accent}`,
        verticalAlign: "middle",
      }}
    />
  );
};

// Terminal window wrapper
const Terminal: React.FC<{
  title: string;
  children: React.ReactNode;
  width?: number;
  opacity?: number;
  scale?: number;
  glow?: boolean;
}> = ({ title, children, width = 800, opacity = 1, scale = 1, glow = false }) => {
  return (
    <div
      style={{
        background: "linear-gradient(180deg, #0a0e14 0%, #060a0f 100%)",
        border: `1px solid ${glow ? "rgba(42, 242, 255, 0.3)" : "#1a2a42"}`,
        borderRadius: 12,
        fontFamily: theme.fontMono,
        fontSize: 22,
        lineHeight: 1.8,
        opacity,
        transform: `scale(${scale})`,
        boxShadow: glow
          ? `
            0 0 0 1px rgba(0, 212, 255, 0.25),
            0 0 60px rgba(0, 212, 255, 0.2),
            0 30px 80px -20px rgba(0, 0, 0, 0.9)
          `
          : `
            0 0 0 1px rgba(0, 212, 255, 0.1),
            0 20px 60px -20px rgba(0, 0, 0, 0.8)
          `,
        width,
        overflow: "hidden",
      }}
    >
      {/* Terminal header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "12px 16px",
          background: "linear-gradient(180deg, #12181f 0%, #0d1218 100%)",
          borderBottom: "1px solid #1a2a42",
        }}
      >
        <div
          style={{
            width: 12,
            height: 12,
            borderRadius: "50%",
            background: "#ff5f57",
            boxShadow: "0 0 8px rgba(255, 95, 87, 0.5)",
          }}
        />
        <div
          style={{
            width: 12,
            height: 12,
            borderRadius: "50%",
            background: "#febc2e",
            boxShadow: "0 0 8px rgba(254, 188, 46, 0.5)",
          }}
        />
        <div
          style={{
            width: 12,
            height: 12,
            borderRadius: "50%",
            background: "#28c840",
            boxShadow: "0 0 8px rgba(40, 200, 64, 0.5)",
          }}
        />
        <span
          style={{
            flex: 1,
            textAlign: "center",
            fontSize: 14,
            color: "#64748b",
            letterSpacing: "0.05em",
          }}
        >
          {title}
        </span>
      </div>

      {/* Terminal content */}
      <div style={{ padding: "20px 24px" }}>{children}</div>
    </div>
  );
};

// Hero text with glow
const HeroText: React.FC<{
  children: string;
  delay?: number;
  fontSize?: number;
  subtle?: boolean;
}> = ({ children, delay = 0, fontSize = 64, subtle = false }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const progress = spring({
    frame: frame - delay,
    fps,
    config: { damping: 20, stiffness: 100 },
  });

  const opacity = interpolate(progress, [0, 1], [0, 1], {
    extrapolateRight: "clamp",
  });
  const translateY = interpolate(progress, [0, 1], [30, 0], {
    extrapolateRight: "clamp",
  });

  const glowIntensity = subtle ? 0.4 : interpolate(
    Math.sin((frame - delay) * 0.1),
    [-1, 1],
    [0.6, 1]
  );

  return (
    <div
      style={{
        fontFamily: theme.fontSans,
        fontSize,
        fontWeight: 700,
        color: subtle ? theme.mutedForeground : "rgba(150, 230, 255, 0.95)",
        textShadow: subtle
          ? "none"
          : `
            0 0 5px rgba(255, 255, 255, ${glowIntensity}),
            0 0 28px rgba(0, 180, 255, ${0.9 * glowIntensity}),
            0 0 50px rgba(0, 150, 255, ${0.5 * glowIntensity})
          `,
        opacity,
        transform: `translateY(${translateY}px)`,
        letterSpacing: "-0.02em",
        textAlign: "center",
        maxWidth: 1400,
      }}
    >
      {children}
    </div>
  );
};

// ============================================================================
// SCENE 1: INSTANT HOOK (0-3s / 0-90 frames)
// ============================================================================

const Scene1_InstantHook: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  // Flash effect at start
  const flashOpacity = interpolate(frame, [0, 3, 8], [1, 0.8, 0], {
    extrapolateRight: "clamp",
  });

  // Query text types instantly
  const queryText = "db.from(alice).out(Knows).toArray()";
  const typedChars = Math.min(
    queryText.length,
    Math.floor(interpolate(frame, [8, 25], [0, queryText.length], {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
      easing: Easing.out(Easing.cubic),
    }))
  );

  // Result appears FAST - before viewer expects
  const showResult = frame > 28;
  const resultOpacity = interpolate(frame, [28, 35], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  // Terminal entry
  const terminalProgress = spring({
    frame: frame - 5,
    fps,
    config: { damping: 20, stiffness: 120 },
  });
  const terminalOpacity = interpolate(terminalProgress, [0, 1], [0, 1], {
    extrapolateRight: "clamp",
  });
  const terminalScale = interpolate(terminalProgress, [0, 1], [0.95, 1], {
    extrapolateRight: "clamp",
  });

  // Text entry
  const textProgress = spring({
    frame: frame - 45,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  return (
    <AbsoluteFill
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 50,
      }}
    >
      {/* Initial flash */}
      <div
        style={{
          position: "absolute",
          inset: 0,
          background: theme.accent,
          opacity: flashOpacity,
          pointerEvents: "none",
        }}
      />

      {/* Terminal with instant query */}
      <Terminal
        title="kitedb — query"
        width={850}
        opacity={terminalOpacity}
        scale={terminalScale}
        glow={showResult}
      >
        <div style={{ display: "flex", alignItems: "center" }}>
          <span style={{ color: theme.accent, marginRight: 12 }}>❯</span>
          <span style={{ color: theme.foreground }}>
            {queryText.slice(0, typedChars)}
          </span>
          <Cursor frame={frame} visible={!showResult} />
        </div>

        {showResult && (
          <div
            style={{
              marginTop: 16,
              opacity: resultOpacity,
              color: theme.accentStrong,
              fontWeight: 600,
            }}
          >
            <span style={{ color: theme.terminalGreen }}>✓</span>{" "}
            <span style={{ color: "#64748b" }}>3 results in</span>{" "}
            <span style={{ color: theme.accent, textShadow: `0 0 10px ${theme.accent}` }}>
              0.12ms
            </span>
          </div>
        )}
      </Terminal>

      {/* Hook text */}
      <div
        style={{
          opacity: interpolate(textProgress, [0, 1], [0, 1], {
            extrapolateRight: "clamp",
          }),
          transform: `translateY(${interpolate(textProgress, [0, 1], [20, 0], {
            extrapolateRight: "clamp",
          })}px)`,
        }}
      >
        <HeroText fontSize={72}>Databases shouldn't feel slow.</HeroText>
      </div>
    </AbsoluteFill>
  );
};

// ============================================================================
// SCENE 2: SPEED PROOF (3-8s / 90-240 frames)
// ============================================================================

const SpeedComparisonTerminal: React.FC<{
  title: string;
  isKite: boolean;
  delay: number;
}> = ({ title, isKite, delay }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const entryProgress = spring({
    frame: frame - delay,
    fps,
    config: { damping: 20, stiffness: 100 },
  });

  const opacity = interpolate(entryProgress, [0, 1], [0, 1], {
    extrapolateRight: "clamp",
  });

  // Typing animation
  const query = isKite
    ? "db.from(user).out(Follows).limit(100)"
    : "SELECT * FROM follows WHERE user_id = ? LIMIT 100";

  const typedChars = Math.floor(
    interpolate(frame - delay, [10, 40], [0, query.length], {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    })
  );

  // Results timing - KiteDB is instant, generic has delay
  const resultDelay = isKite ? 45 : 90;
  const showResults = frame - delay > resultDelay;

  const resultOpacity = interpolate(
    frame - delay,
    [resultDelay, resultDelay + 10],
    [0, 1],
    {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    }
  );

  // Loading spinner for generic DB
  const showLoading = !isKite && frame - delay > 45 && frame - delay < 90;
  const spinnerRotation = (frame - delay) * 8;

  // Results cascade in
  const results = ["Alice Chen", "Bob Wilson", "Carol Davis", "Dan Smith"];
  const resultLines = results.map((name, i) => {
    const lineDelay = resultDelay + 5 + i * 5;
    const lineOpacity = interpolate(frame - delay, [lineDelay, lineDelay + 5], [0, 1], {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    });
    const lineTranslate = interpolate(frame - delay, [lineDelay, lineDelay + 5], [10, 0], {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    });
    return { name, opacity: lineOpacity, translate: lineTranslate };
  });

  return (
    <div style={{ opacity }}>
      <Terminal title={title} width={700} glow={isKite && showResults}>
        <div style={{ display: "flex", alignItems: "center" }}>
          <span style={{ color: theme.accent, marginRight: 12 }}>❯</span>
          <span style={{ color: theme.foreground, fontSize: 18 }}>
            {query.slice(0, typedChars)}
          </span>
          <Cursor frame={frame} visible={typedChars < query.length} />
        </div>

        {/* Loading state for generic */}
        {showLoading && (
          <div style={{ marginTop: 16, display: "flex", alignItems: "center", gap: 10 }}>
            <div
              style={{
                width: 16,
                height: 16,
                border: "2px solid #64748b",
                borderTopColor: "transparent",
                borderRadius: "50%",
                transform: `rotate(${spinnerRotation}deg)`,
              }}
            />
            <span style={{ color: "#64748b" }}>Loading...</span>
          </div>
        )}

        {/* Results */}
        {showResults && (
          <div style={{ marginTop: 16, opacity: resultOpacity }}>
            {resultLines.map((line) => (
              <div
                key={line.name}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 10,
                  opacity: line.opacity,
                  transform: `translateX(${line.translate}px)`,
                  marginBottom: 4,
                }}
              >
                <span style={{ color: isKite ? theme.terminalGreen : "#64748b" }}>
                  {isKite ? "●" : "○"}
                </span>
                <span style={{ color: theme.foreground }}>{line.name}</span>
              </div>
            ))}
            <div
              style={{
                marginTop: 12,
                color: isKite ? theme.accentStrong : "#64748b",
                fontSize: 16,
              }}
            >
              {isKite ? "✓ 0.08ms" : "⏱ 127ms"}
            </div>
          </div>
        )}
      </Terminal>

      {/* Label */}
      <div
        style={{
          textAlign: "center",
          marginTop: 16,
          fontFamily: theme.fontSans,
          fontSize: 18,
          color: isKite ? theme.accent : "#64748b",
          fontWeight: isKite ? 600 : 400,
        }}
      >
        {isKite ? "KiteDB" : "Traditional DB"}
      </div>
    </div>
  );
};

const Scene2_SpeedProof: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const textProgress = spring({
    frame: frame - 100,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  return (
    <AbsoluteFill
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 40,
      }}
    >
      {/* Side by side terminals */}
      <div style={{ display: "flex", gap: 60 }}>
        <SpeedComparisonTerminal title="database — query" isKite={false} delay={5} />
        <SpeedComparisonTerminal title="kitedb — query" isKite={true} delay={10} />
      </div>

      {/* Subtitle */}
      <div
        style={{
          opacity: interpolate(textProgress, [0, 1], [0, 1], {
            extrapolateRight: "clamp",
          }),
          transform: `translateY(${interpolate(textProgress, [0, 1], [15, 0], {
            extrapolateRight: "clamp",
          })}px)`,
        }}
      >
        <HeroText fontSize={40} subtle>
          Instant reads. Zero friction.
        </HeroText>
      </div>
    </AbsoluteFill>
  );
};

// ============================================================================
// SCENE 3: FLUENT QUERY SYNTAX (8-14s / 240-420 frames)
// ============================================================================

const Scene3_FluentSyntax: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const fullCode = `const db = await kite('./social.kitedb')

// Traverse relationships fluently
const friends = db
  .from(alice)
  .out(Knows)
  .where(n => n.get("active"))
  .toArray()

// Find shortest path
const path = db
  .shortestPath(alice).to(bob)
  .via(Knows)
  .dijkstra()`;

  // Typewriter effect
  const typingSpeed = 2;
  const typedChars = Math.floor(
    interpolate(frame, [15, 15 + fullCode.length / typingSpeed], [0, fullCode.length], {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    })
  );

  const displayedCode = fullCode.slice(0, typedChars);

  // Syntax highlighting
  const highlightCode = (code: string) => {
    const tokens: { text: string; type: string }[] = [];
    let remaining = code;

    const keywords = ["const", "await"];
    const functions = ["kite", "from", "out", "where", "toArray", "shortestPath", "to", "via", "dijkstra", "get"];
    const types = ["Knows"];
    const variables = ["db", "friends", "alice", "bob", "path", "n"];

    while (remaining.length > 0) {
      // Comments
      const commentMatch = remaining.match(/^\/\/[^\n]*/);
      if (commentMatch) {
        tokens.push({ text: commentMatch[0], type: "comment" });
        remaining = remaining.slice(commentMatch[0].length);
        continue;
      }

      // Strings
      const stringMatch = remaining.match(/^'[^']*'?/);
      if (stringMatch) {
        tokens.push({ text: stringMatch[0], type: "string" });
        remaining = remaining.slice(stringMatch[0].length);
        continue;
      }

      // Arrow function
      const arrowMatch = remaining.match(/^=>/);
      if (arrowMatch) {
        tokens.push({ text: "=>", type: "punctuation" });
        remaining = remaining.slice(2);
        continue;
      }

      // Words
      const wordMatch = remaining.match(/^[a-zA-Z_][a-zA-Z0-9_]*/);
      if (wordMatch) {
        const word = wordMatch[0];
        let type = "default";
        if (keywords.includes(word)) type = "keyword";
        else if (functions.includes(word)) type = "function";
        else if (types.includes(word)) type = "type";
        else if (variables.includes(word)) type = "variable";
        tokens.push({ text: word, type });
        remaining = remaining.slice(word.length);
        continue;
      }

      tokens.push({ text: remaining[0], type: "punctuation" });
      remaining = remaining.slice(1);
    }

    return tokens;
  };

  const tokens = highlightCode(displayedCode);

  const getColor = (type: string) => {
    switch (type) {
      case "keyword": return theme.codeKeyword;
      case "function": return theme.codeFunction;
      case "type": return theme.codeType;
      case "variable": return theme.codeVariable;
      case "string": return theme.codeString;
      case "comment": return theme.codeComment;
      default: return theme.mutedForeground;
    }
  };

  // Terminal entry
  const entryProgress = spring({
    frame,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  const opacity = interpolate(entryProgress, [0, 1], [0, 1], {
    extrapolateRight: "clamp",
  });
  const scale = interpolate(entryProgress, [0, 1], [0.95, 1], {
    extrapolateRight: "clamp",
  });

  // Hero text
  const textProgress = spring({
    frame: frame - 30,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  return (
    <AbsoluteFill
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 40,
      }}
    >
      {/* Code editor */}
      <div style={{ opacity, transform: `scale(${scale})` }}>
        <Terminal title="app.ts — TypeScript" width={900} glow>
          <pre style={{ margin: 0, whiteSpace: "pre-wrap", fontSize: 24, lineHeight: 1.6 }}>
            {tokens.map((token, i) => (
              <span key={`${i}-${token.text}`} style={{ color: getColor(token.type) }}>
                {token.text}
              </span>
            ))}
            <Cursor frame={frame} visible={typedChars < fullCode.length} />
          </pre>
        </Terminal>
      </div>

      {/* Subtitle */}
      <div
        style={{
          opacity: interpolate(textProgress, [0, 1], [0, 1], {
            extrapolateRight: "clamp",
          }),
          transform: `translateY(${interpolate(textProgress, [0, 1], [15, 0], {
            extrapolateRight: "clamp",
          })}px)`,
        }}
      >
        <HeroText fontSize={44}>Queries that read like thought.</HeroText>
      </div>
    </AbsoluteFill>
  );
};

// ============================================================================
// SCENE 4: DEVELOPER FLOW (14-20s / 420-600 frames)
// ============================================================================

const FlowSnippet: React.FC<{
  code: string;
  result: string;
  delay: number;
  position: { x: number; y: number };
}> = ({ code, result, delay, position }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const entryProgress = spring({
    frame: frame - delay,
    fps,
    config: { damping: 20, stiffness: 150 },
  });

  const opacity = interpolate(entryProgress, [0, 1], [0, 1], {
    extrapolateRight: "clamp",
  });
  const translateY = interpolate(entryProgress, [0, 1], [30, 0], {
    extrapolateRight: "clamp",
  });

  // Result appears after typing
  const showResult = frame - delay > 25;
  const resultOpacity = interpolate(frame - delay, [25, 35], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  // Exit animation
  const exitProgress = interpolate(frame - delay, [50, 60], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const exitOpacity = 1 - exitProgress;

  return (
    <div
      style={{
        position: "absolute",
        left: position.x,
        top: position.y,
        opacity: opacity * exitOpacity,
        transform: `translateY(${translateY}px)`,
      }}
    >
      <div
        style={{
          background: "rgba(10, 14, 20, 0.95)",
          border: "1px solid rgba(42, 242, 255, 0.2)",
          borderRadius: 8,
          padding: "12px 16px",
          fontFamily: theme.fontMono,
          fontSize: 18,
        }}
      >
        <div style={{ color: theme.codeFunction }}>{code}</div>
        {showResult && (
          <div style={{ color: theme.accentStrong, marginTop: 8, opacity: resultOpacity }}>
            → {result}
          </div>
        )}
      </div>
    </div>
  );
};

const Scene4_DeveloperFlow: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const snippets = [
    { code: ".filter(n => n.age > 25)", result: "847 nodes", delay: 0, position: { x: 200, y: 200 } },
    { code: ".out(WorksAt)", result: "traversed 3.2K edges", delay: 30, position: { x: 600, y: 350 } },
    { code: ".unique()", result: "312 companies", delay: 60, position: { x: 300, y: 500 } },
    { code: ".orderBy('revenue')", result: "sorted in 0.4ms", delay: 90, position: { x: 700, y: 250 } },
    { code: ".take(10).toArray()", result: "done ✓", delay: 120, position: { x: 450, y: 400 } },
  ];

  // Center text
  const textProgress = spring({
    frame: frame - 60,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  return (
    <AbsoluteFill>
      {/* Rapid snippets flying in */}
      {snippets.map((snippet) => (
        <FlowSnippet key={snippet.code} {...snippet} />
      ))}

      {/* Center text */}
      <AbsoluteFill
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <div
          style={{
            opacity: interpolate(textProgress, [0, 1], [0, 1], {
              extrapolateRight: "clamp",
            }),
            transform: `scale(${interpolate(textProgress, [0, 1], [0.9, 1], {
              extrapolateRight: "clamp",
            })})`,
          }}
        >
          <HeroText fontSize={80}>Stay in flow.</HeroText>
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};

// ============================================================================
// SCENE 5: BUILT FOR SPEED (20-25s / 600-750 frames)
// ============================================================================

const SpeedParticle: React.FC<{
  startX: number;
  startY: number;
  speed: number;
  delay: number;
  length: number;
}> = ({ startX, startY, speed, delay, length }) => {
  const frame = useCurrentFrame();

  const progress = ((frame - delay) * speed) % 2000;
  const x = startX + progress;
  const opacity = interpolate(progress, [0, 100, 1800, 2000], [0, 0.6, 0.6, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  return (
    <div
      style={{
        position: "absolute",
        left: x - length,
        top: startY,
        width: length,
        height: 2,
        background: `linear-gradient(90deg, transparent, ${theme.accent})`,
        opacity,
        boxShadow: `0 0 8px ${theme.accent}`,
      }}
    />
  );
};

const Scene5_Performance: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  // Generate particles
  const particles = Array.from({ length: 20 }, (_, i) => ({
    startX: -200 - (i * 100),
    startY: 100 + (i * 45),
    speed: 8 + (i % 5) * 2,
    delay: i * 3,
    length: 80 + (i % 3) * 40,
  }));

  // Metrics that fade in
  const metrics = [
    { label: "Zero-copy mmap", delay: 20 },
    { label: "ACID transactions", delay: 35 },
    { label: "Single file storage", delay: 50 },
  ];

  // Text
  const textProgress = spring({
    frame: frame - 10,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  return (
    <AbsoluteFill>
      {/* Speed particles */}
      {particles.map((p) => (
        <SpeedParticle key={`${p.startX}-${p.startY}`} {...p} />
      ))}

      {/* Content */}
      <AbsoluteFill
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          gap: 50,
        }}
      >
        <div
          style={{
            opacity: interpolate(textProgress, [0, 1], [0, 1], {
              extrapolateRight: "clamp",
            }),
            transform: `translateY(${interpolate(textProgress, [0, 1], [20, 0], {
              extrapolateRight: "clamp",
            })}px)`,
          }}
        >
          <HeroText fontSize={72}>Designed for performance.</HeroText>
        </div>

        {/* Metrics */}
        <div style={{ display: "flex", gap: 60 }}>
          {metrics.map((metric, i) => {
            const metricProgress = spring({
              frame: frame - metric.delay,
              fps,
              config: { damping: 20, stiffness: 100 },
            });
            const metricOpacity = interpolate(metricProgress, [0, 1], [0, 1], {
              extrapolateRight: "clamp",
            });
            const metricTranslate = interpolate(metricProgress, [0, 1], [15, 0], {
              extrapolateRight: "clamp",
            });

            return (
              <div
                key={metric.label}
                style={{
                  opacity: metricOpacity,
                  transform: `translateY(${metricTranslate}px)`,
                  fontFamily: theme.fontMono,
                  fontSize: 22,
                  color: theme.mutedForeground,
                  padding: "12px 24px",
                  border: `1px solid ${theme.border}`,
                  borderRadius: 8,
                  background: "rgba(10, 14, 20, 0.6)",
                }}
              >
                {metric.label}
              </div>
            );
          })}
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};

// ============================================================================
// SCENE 6: INSTALLATION + END CARD (25-30s / 750-900 frames)
// ============================================================================

const Scene6_EndCard: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  // Terminal typing
  const command = "npm install @kitedb/core";
  const typedChars = Math.floor(
    interpolate(frame, [20, 50], [0, command.length], {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    })
  );

  // Entry animations
  const terminalProgress = spring({
    frame: frame - 5,
    fps,
    config: { damping: 20, stiffness: 100 },
  });

  const logoProgress = spring({
    frame: frame - 60,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  const taglineProgress = spring({
    frame: frame - 75,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  const urlProgress = spring({
    frame: frame - 90,
    fps,
    config: { damping: 15, stiffness: 80 },
  });

  // Show success after typing
  const showSuccess = frame > 55;
  const successOpacity = interpolate(frame, [55, 60], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  return (
    <AbsoluteFill
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 30,
      }}
    >
      {/* Install command */}
      <div
        style={{
          opacity: interpolate(terminalProgress, [0, 1], [0, 1], {
            extrapolateRight: "clamp",
          }),
          transform: `scale(${interpolate(terminalProgress, [0, 1], [0.95, 1], {
            extrapolateRight: "clamp",
          })})`,
        }}
      >
        <Terminal title="terminal" width={600}>
          <div style={{ display: "flex", alignItems: "center" }}>
            <span style={{ color: theme.accent, marginRight: 12 }}>❯</span>
            <span style={{ color: theme.foreground }}>
              {command.slice(0, typedChars)}
            </span>
            <Cursor frame={frame} visible={!showSuccess} />
          </div>
          {showSuccess && (
            <div style={{ marginTop: 12, opacity: successOpacity }}>
              <span style={{ color: theme.terminalGreen }}>✓</span>{" "}
              <span style={{ color: "#64748b" }}>added 1 package</span>
            </div>
          )}
        </Terminal>
      </div>

      {/* Tagline */}
      <div
        style={{
          opacity: interpolate(taglineProgress, [0, 1], [0, 1], {
            extrapolateRight: "clamp",
          }),
          transform: `translateY(${interpolate(taglineProgress, [0, 1], [15, 0], {
            extrapolateRight: "clamp",
          })}px)`,
          fontFamily: theme.fontSans,
          fontSize: 36,
          color: theme.foreground,
          fontWeight: 600,
          letterSpacing: "0.05em",
        }}
      >
        Install. Query. Ship.
      </div>

      {/* Logo + KiteDB */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 24,
          opacity: interpolate(logoProgress, [0, 1], [0, 1], {
            extrapolateRight: "clamp",
          }),
          transform: `scale(${interpolate(logoProgress, [0, 1], [0.8, 1], {
            extrapolateRight: "clamp",
          })})`,
        }}
      >
        <KiteLogo scale={1.2} delay={60} />
        <div
          style={{
            fontFamily: theme.fontSans,
            fontSize: 64,
            fontWeight: 700,
            color: "rgba(150, 230, 255, 0.95)",
            textShadow: `
              0 0 5px rgba(255, 255, 255, 0.8),
              0 0 28px rgba(0, 180, 255, 0.9),
              0 0 50px rgba(0, 150, 255, 0.5)
            `,
          }}
        >
          KiteDB
        </div>
      </div>

      {/* URL */}
      <div
        style={{
          opacity: interpolate(urlProgress, [0, 1], [0, 1], {
            extrapolateRight: "clamp",
          }),
          transform: `translateY(${interpolate(urlProgress, [0, 1], [10, 0], {
            extrapolateRight: "clamp",
          })}px)`,
          fontFamily: theme.fontMono,
          fontSize: 28,
          color: theme.accent,
          textShadow: `0 0 15px ${theme.accent}`,
          marginTop: 10,
        }}
      >
        kitedb.io
      </div>
    </AbsoluteFill>
  );
};

// ============================================================================
// MAIN COMPOSITION
// ============================================================================

export const KiteDBPromo: React.FC = () => {
  const { fps } = useVideoConfig();

  return (
    <AbsoluteFill style={{ backgroundColor: theme.background }}>
      <Background />

      {/* Scene 1: Instant Hook (0-3s) */}
      <Sequence from={0} durationInFrames={3 * fps} name="Hook">
        <Scene1_InstantHook />
      </Sequence>

      {/* Scene 2: Speed Proof (3-8s) */}
      <Sequence from={3 * fps} durationInFrames={5 * fps} name="SpeedProof">
        <Scene2_SpeedProof />
      </Sequence>

      {/* Scene 3: Fluent Query Syntax (8-14s) */}
      <Sequence from={8 * fps} durationInFrames={6 * fps} name="FluentSyntax">
        <Scene3_FluentSyntax />
      </Sequence>

      {/* Scene 4: Developer Flow (14-20s) */}
      <Sequence from={14 * fps} durationInFrames={6 * fps} name="DeveloperFlow">
        <Scene4_DeveloperFlow />
      </Sequence>

      {/* Scene 5: Built for Speed (20-25s) */}
      <Sequence from={20 * fps} durationInFrames={5 * fps} name="Performance">
        <Scene5_Performance />
      </Sequence>

      {/* Scene 6: Installation + End Card (25-30s) */}
      <Sequence from={25 * fps} durationInFrames={5 * fps} name="EndCard">
        <Scene6_EndCard />
      </Sequence>
    </AbsoluteFill>
  );
};
