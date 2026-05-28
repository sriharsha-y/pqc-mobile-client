/**
 * PQC Mobile Client — RN integration sample.
 *
 * Demonstrates the post-quantum TLS handshake AND how to *verify* it
 * reliably. A single toggle controls whether the native Rust client
 * advertises X25519MLKEM768 (post-quantum) or classical-only groups,
 * then the app fetches Cloudflare's trace endpoint and reads the
 * SERVER's report of the key exchange it negotiated:
 *
 *   https://pq.cloudflareresearch.com/cdn-cgi/trace  →  "kex=..." line
 *
 * Why the server's `kex=` and not a client-side header? The negotiated
 * group is a property of the TLS *connection*, not the request, and our
 * Rust core can only observe it through a process-global side-channel
 * that races under concurrent requests. Cloudflare's `/cdn-cgi/trace`
 * reports what the edge actually negotiated for *this* connection, in
 * the response body — server-authoritative and correct even under
 * parallel requests. See docs/ios.md and docs/android.md.
 *
 * Toggle ON  → client offers X25519MLKEM768 (+ classical) → kex=X25519MLKEM768
 * Toggle OFF → client offers classical only               → kex=X25519
 *
 * The native side (PqcInterceptor on Android / PqcURLProtocol on iOS)
 * keeps two clients — PQC-on and PQC-off — and selects between them
 * based on the `X-Pqc-Mode` request header this screen sets.
 *
 * iOS 26+ note: AppDelegate gates PqcURLProtocol off because Apple's
 * native URLSession already negotiates PQC. On that path the Rust client
 * is not in the request path and the toggle has no effect, so the UI
 * disables the toggle and shows an explanatory banner; `kex` reflects the
 * OS handshake.
 */

import React, {useCallback, useEffect, useState} from 'react';
import {
  ActivityIndicator,
  Platform,
  SafeAreaView,
  ScrollView,
  StatusBar,
  StyleSheet,
  Switch,
  Text,
  View,
} from 'react-native';

// Any Cloudflare-served host exposes /cdn-cgi/trace; the research
// endpoint is the documented one for PQC testing.
const TRACE_URL = 'https://pq.cloudflareresearch.com/cdn-cgi/trace';

// Opt-in header the native layer routes on: "off" selects the
// classical-only client; absent/anything-else selects the PQC client.
const PQC_MODE_HEADER = 'X-Pqc-Mode';

type Result =
  | {status: 'idle'}
  | {status: 'loading'}
  | {status: 'ok'; httpStatus: number; kex: string | null; raw: string}
  | {status: 'error'; message: string};

/** Pull the `kex=...` value out of a /cdn-cgi/trace body. */
function parseKex(traceBody: string): string | null {
  const match = traceBody.match(/^kex=(.*)$/m);
  return match ? match[1].trim() : null;
}

function isPostQuantum(kex: string | null): boolean {
  // X25519MLKEM768 today; match the ML-KEM family defensively in case
  // Cloudflare relabels (e.g. a future SecP256r1MLKEM768).
  return !!kex && kex.toUpperCase().includes('MLKEM');
}

export default function App(): React.JSX.Element {
  const [pqcEnabled, setPqcEnabled] = useState(true);
  const [result, setResult] = useState<Result>({status: 'idle'});

  // iOS 26+ negotiates X25519MLKEM768 natively in URLSession, so
  // AppDelegate.mm does NOT register PqcURLProtocol there — fetch() goes
  // through the system stack and the X-Pqc-Mode header is ignored. The
  // toggle is therefore inert on iOS 26+, so we disable it and explain.
  const iosNativePqc =
    Platform.OS === 'ios' &&
    Number.parseInt(String(Platform.Version), 10) >= 26;

  const run = useCallback(async (enablePqc: boolean) => {
    setResult({status: 'loading'});
    try {
      const resp = await fetch(TRACE_URL, {
        method: 'GET',
        // When PQC is toggled OFF, tell the native layer to route through
        // the classical-only client. When ON, send nothing special.
        headers: enablePqc ? {} : {[PQC_MODE_HEADER]: 'off'},
      });
      const raw = await resp.text();
      setResult({
        status: 'ok',
        httpStatus: resp.status,
        kex: parseKex(raw),
        raw,
      });
    } catch (err: unknown) {
      setResult({
        status: 'error',
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }, []);

  // Run on mount and whenever the toggle flips.
  useEffect(() => {
    run(pqcEnabled);
  }, [pqcEnabled, run]);

  return (
    <SafeAreaView style={styles.root}>
      <StatusBar barStyle="default" />
      <ScrollView contentContainerStyle={styles.scroll}>
        <Text style={styles.appTitle}>pqc-mobile-client</Text>
        <Text style={styles.appSubtitle}>
          Platform: {Platform.OS}
          {Platform.OS === 'ios' ? ` ${String(Platform.Version)}` : ''}
        </Text>

        {iosNativePqc && (
          <View style={styles.banner}>
            <Text style={styles.bannerText}>
              iOS 26+ negotiates X25519MLKEM768 natively via URLSession, so the
              Rust PQC client (PqcURLProtocol) is not installed on this OS. This
              toggle has no effect here — the result below reflects the system
              handshake.
            </Text>
          </View>
        )}

        <View style={styles.toggleRow}>
          <View style={styles.toggleText}>
            <Text style={styles.toggleLabel}>Advertise post-quantum</Text>
            <Text style={styles.toggleCaption}>
              {iosNativePqc
                ? 'Handled natively by iOS 26+ (toggle disabled)'
                : `X25519MLKEM768 ${
                    pqcEnabled ? 'offered' : 'disabled (classical only)'
                  }`}
            </Text>
          </View>
          <Switch
            value={iosNativePqc || pqcEnabled}
            onValueChange={setPqcEnabled}
            disabled={iosNativePqc || result.status === 'loading'}
          />
        </View>

        <ResultCard
          pqcRequested={pqcEnabled}
          iosNativePqc={iosNativePqc}
          result={result}
        />
      </ScrollView>
    </SafeAreaView>
  );
}

function ResultCard({
  pqcRequested,
  iosNativePqc,
  result,
}: {
  pqcRequested: boolean;
  iosNativePqc: boolean;
  result: Result;
}): React.JSX.Element {
  const pqcNegotiated = result.status === 'ok' && isPostQuantum(result.kex);

  return (
    <View style={styles.card}>
      <Text style={styles.cardTitle}>Cloudflare /cdn-cgi/trace</Text>
      <Text style={styles.cardUrl}>{TRACE_URL}</Text>

      {result.status === 'idle' && <Text style={styles.muted}>idle</Text>}

      {result.status === 'loading' && (
        <View style={styles.spinner}>
          <ActivityIndicator size="small" />
          <Text style={styles.muted}>Performing TLS handshake…</Text>
        </View>
      )}

      {result.status === 'ok' && (
        <>
          <Text style={styles.label}>Negotiated KEX (server-reported)</Text>
          {result.kex === null ? (
            <>
              <Text style={[styles.value, styles.kexUnknown]}>not reported</Text>
              <Text style={styles.caption}>
                No `kex=` line in the trace body — endpoint may not be
                Cloudflare-served.
              </Text>
            </>
          ) : (
            <>
              <Text
                style={[
                  styles.value,
                  pqcNegotiated ? styles.kexPqc : styles.kexClassical,
                ]}>
                {result.kex}
                {pqcNegotiated ? '  ✓ post-quantum' : '  (classical)'}
              </Text>
              <Text style={styles.caption}>
                {iosNativePqc
                  ? 'Negotiated natively by iOS 26+ URLSession (Rust client not in the path).'
                  : pqcRequested
                  ? pqcNegotiated
                    ? 'PQC offered and negotiated — confirmed by the edge.'
                    : 'PQC offered but the edge selected classical (graceful downgrade).'
                  : 'PQC disabled on the client — classical handshake as expected.'}
              </Text>
            </>
          )}
          <Text style={styles.label}>HTTP status</Text>
          <Text style={styles.value}>{result.httpStatus}</Text>
        </>
      )}

      {result.status === 'error' && (
        <>
          <Text style={styles.label}>Error</Text>
          <Text style={styles.error}>{result.message}</Text>
        </>
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  root: {flex: 1, backgroundColor: '#0b0d11'},
  scroll: {padding: 16, paddingTop: 24},
  appTitle: {color: '#e7eaf0', fontSize: 22, fontWeight: '600'},
  appSubtitle: {color: '#5d97f7', fontSize: 13, marginTop: 4, marginBottom: 18},
  toggleRow: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    backgroundColor: '#161a22',
    borderRadius: 14,
    padding: 16,
    marginBottom: 12,
  },
  toggleText: {flex: 1, paddingRight: 12},
  toggleLabel: {color: '#e7eaf0', fontSize: 16, fontWeight: '600'},
  toggleCaption: {color: '#7d8595', fontSize: 12, marginTop: 2},
  banner: {
    backgroundColor: '#1d2535',
    borderRadius: 12,
    padding: 14,
    marginBottom: 12,
    borderLeftWidth: 3,
    borderLeftColor: '#5d97f7',
  },
  bannerText: {color: '#aab6cf', fontSize: 12, lineHeight: 17},
  card: {
    backgroundColor: '#161a22',
    borderRadius: 14,
    padding: 20,
    marginBottom: 14,
  },
  cardTitle: {color: '#e7eaf0', fontSize: 16, fontWeight: '600'},
  cardUrl: {color: '#7d8595', fontSize: 12, marginTop: 2, marginBottom: 10},
  label: {color: '#7d8595', fontSize: 12, marginTop: 12, textTransform: 'uppercase'},
  value: {
    color: '#e7eaf0',
    fontSize: 16,
    fontFamily: Platform.select({ios: 'Menlo', default: 'monospace'}),
  },
  caption: {color: '#7d8595', fontSize: 12, marginTop: 4, fontStyle: 'italic'},
  error: {color: '#ff6f6f', fontSize: 13, marginTop: 4},
  muted: {color: '#7d8595', fontSize: 13, marginTop: 8},
  spinner: {flexDirection: 'row', alignItems: 'center', marginTop: 8},
  kexPqc: {color: '#5dd193'},
  kexClassical: {color: '#e8b94c'},
  kexUnknown: {color: '#7d8595'},
});
