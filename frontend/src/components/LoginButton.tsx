import { useEffect, useRef, useState } from "react";
import { LogIn, LogOut, User } from "lucide-react";
import { send, subscribe } from "../hooks/useIPC";

/// Shape of the `sso_state` envelope from the Rust side. Two modes:
///
/// - `managed: true` — an enterprise policy file pinned thClaws to a
///   specific IdP. No provider picker; one "Sign in" button kicks off
///   the org's flow.
/// - `managed: false` (standard) — `providers` lists which built-in
///   SSO buttons to render (today: just `google` while Azure waits on
///   its OAuth registration). Empty `providers` means no env vars are
///   set, so the button surfaces a configure-hint instead.
type SsoState = {
  enabled?: boolean;
  managed?: boolean;
  logged_in?: boolean;
  email?: string | null;
  name?: string | null;
  issuer?: string | null;
  provider?: string | null;
  expires_in_secs?: number | null;
  providers?: { id: string; label: string }[];
  error?: string | null;
};

/// Navbar Login control. Right side of the tab bar. Three visual
/// states:
/// 1. Logged out + providers available → "Sign in" → dropdown picker.
/// 2. Logged out + no providers configured → "Sign in" → tooltip
///    explaining the env-var setup.
/// 3. Logged in → user icon + email → dropdown with "Sign out".
export function LoginButton() {
  const [state, setState] = useState<SsoState | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  // Initial state + live updates. The backend re-emits sso_state after
  // login / logout completes — but the post-login dispatch arrives
  // while the user is still over in the browser tab, and the React
  // update can race with the focus shift. To self-heal: also refetch
  // sso_status whenever the window regains focus (the natural "I came
  // back from OAuth" moment) and whenever the user opens the dropdown.
  useEffect(() => {
    const unsub = subscribe((msg) => {
      if (msg.type === "sso_state") {
        const next = msg as SsoState;
        setState(next);
        setBusy(false);
        // Surface login failures: the OAuth flow can complete in the
        // browser but the desktop side still error (keychain refused,
        // network glitch on the token exchange, etc.). Pre-fix the
        // error sat inside the closed dropdown so the user clicked
        // "Sign in" repeatedly without seeing why it kept failing.
        if (next.error) setMenuOpen(true);
      }
    });
    send({ type: "sso_status" });
    const onFocus = () => send({ type: "sso_status" });
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
      unsub();
    };
  }, []);

  // Refetch state every time the dropdown opens so the user never
  // sees a stale "Sign in" label after a successful login that the
  // initial dispatch missed.
  useEffect(() => {
    if (menuOpen) send({ type: "sso_status" });
  }, [menuOpen]);

  // Click-outside to close the dropdown.
  useEffect(() => {
    if (!menuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    window.addEventListener("mousedown", onClick);
    return () => window.removeEventListener("mousedown", onClick);
  }, [menuOpen]);

  const loggedIn = Boolean(state?.logged_in);
  const managed = Boolean(state?.managed);
  const providers = state?.providers ?? [];
  const displayName =
    state?.email || state?.name || (loggedIn ? "signed in" : "");

  const signIn = (provider?: string) => {
    setBusy(true);
    setMenuOpen(false);
    send({ type: "sso_login", provider });
  };

  const signOut = () => {
    setBusy(true);
    setMenuOpen(false);
    send({ type: "sso_logout" });
  };

  return (
    <div ref={rootRef} className="relative">
      <button
        type="button"
        onClick={() => setMenuOpen((o) => !o)}
        disabled={busy}
        className="flex items-center gap-1.5 px-3 py-1 mr-2 text-xs font-medium rounded transition-colors"
        style={{
          background: loggedIn
            ? "color-mix(in srgb, var(--accent) 14%, transparent)"
            : "transparent",
          color: loggedIn ? "var(--accent)" : "var(--text-secondary)",
          border: `1px solid ${loggedIn ? "color-mix(in srgb, var(--accent) 40%, transparent)" : "var(--border)"}`,
          cursor: busy ? "wait" : "pointer",
        }}
        title={
          loggedIn
            ? `Signed in as ${displayName}${state?.issuer ? ` · ${state.issuer}` : ""}`
            : "Sign in to unlock cloud features"
        }
      >
        {loggedIn ? <User size={12} /> : <LogIn size={12} />}
        <span className="max-w-[160px] truncate">
          {busy ? "Signing in…" : loggedIn ? displayName : "Sign in"}
        </span>
      </button>

      {menuOpen && (
        <div
          className="absolute right-2 top-full mt-1 min-w-[200px] rounded shadow-lg z-50"
          style={{
            background: "var(--bg-secondary)",
            border: "1px solid var(--border)",
          }}
        >
          {loggedIn ? (
            <>
              <div
                className="px-3 py-2 text-xs"
                style={{
                  color: "var(--text-secondary)",
                  borderBottom: "1px solid var(--border)",
                }}
              >
                <div
                  className="truncate"
                  style={{ color: "var(--text-primary)" }}
                >
                  {displayName}
                </div>
                {state?.issuer && (
                  <div className="truncate" style={{ opacity: 0.7 }}>
                    {state.issuer}
                  </div>
                )}
              </div>
              <button
                type="button"
                onClick={signOut}
                className="flex items-center gap-2 w-full text-left px-3 py-2 text-xs"
                style={{ color: "var(--text-primary)" }}
                onMouseEnter={(e) =>
                  (e.currentTarget.style.background = "var(--bg-tertiary)")
                }
                onMouseLeave={(e) =>
                  (e.currentTarget.style.background = "transparent")
                }
              >
                <LogOut size={12} />
                Sign out
              </button>
            </>
          ) : managed ? (
            // Enterprise-managed: a single "Sign in" entry points at
            // the org's IdP. The picker collapses to one option so it
            // mirrors the standard UX without exposing the issuer
            // string in the menu.
            <button
              type="button"
              onClick={() => signIn()}
              className="flex items-center gap-2 w-full text-left px-3 py-2 text-xs"
              style={{ color: "var(--text-primary)" }}
              onMouseEnter={(e) =>
                (e.currentTarget.style.background = "var(--bg-tertiary)")
              }
              onMouseLeave={(e) =>
                (e.currentTarget.style.background = "transparent")
              }
            >
              <LogIn size={12} />
              Sign in
              {state?.issuer && (
                <span style={{ opacity: 0.6 }}>· {hostnameOf(state.issuer)}</span>
              )}
            </button>
          ) : providers.length === 0 ? (
            // No provider env vars set + no bundled credentials in the
            // binary. Surface the configure hint inline so the user
            // knows exactly what to do without trawling docs. Covers
            // both standard providers — Google (id + secret) and
            // Azure (id only; native/public client runs PKCE so no
            // secret is required or accepted).
            <div
              className="px-3 py-2 text-xs leading-relaxed"
              style={{ color: "var(--text-secondary)" }}
            >
              Add either of the following to your{" "}
              <code className="font-mono">.env</code> to enable sign-in:
              <ul className="mt-1.5 ml-3 list-disc">
                <li>
                  <code
                    className="font-mono"
                    style={{ color: "var(--text-primary)" }}
                  >
                    GOOGLE_CLIENT_ID
                  </code>{" "}
                  +{" "}
                  <code
                    className="font-mono"
                    style={{ color: "var(--text-primary)" }}
                  >
                    GOOGLE_CLIENT_SECRET
                  </code>
                </li>
                <li>
                  <code
                    className="font-mono"
                    style={{ color: "var(--text-primary)" }}
                  >
                    AZURE_CLIENT_ID
                  </code>
                </li>
              </ul>
            </div>
          ) : (
            providers.map((p) => (
              <button
                key={p.id}
                type="button"
                onClick={() => signIn(p.id)}
                className="flex items-center gap-2 w-full text-left px-3 py-2 text-xs"
                style={{ color: "var(--text-primary)" }}
                onMouseEnter={(e) =>
                  (e.currentTarget.style.background = "var(--bg-tertiary)")
                }
                onMouseLeave={(e) =>
                  (e.currentTarget.style.background = "transparent")
                }
              >
                <LogIn size={12} />
                Sign in with {p.label}
              </button>
            ))
          )}
          {state?.error && (
            <div
              className="px-3 py-2 text-xs border-t"
              style={{
                color: "#f85149",
                borderColor: "var(--border)",
              }}
            >
              {state.error}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/// Strip the protocol off an issuer URL for a tighter menu label
/// (`https://accounts.google.com` → `accounts.google.com`). Falls
/// back to the raw string when URL parsing fails.
function hostnameOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
