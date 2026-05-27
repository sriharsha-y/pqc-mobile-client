/**
 * PQC Mobile Client — RN integration sample.
 *
 * Fires two HTTPS requests in sequence and reports the negotiated TLS
 * key-exchange group for each via the `X-Pqc-Negotiated-Group` response
 * header (injected by PqcURLProtocol on iOS / PqcInterceptor on Android
 * from the Rust core's `negotiatedNamedGroup`, sourced from
 * `src/kx_tracker.rs` which records the group rustls actually selected).
 *
 *   1. https://pq.cloudflareresearch.com/ — Cloudflare edge advertises
 *      X25519MLKEM768; expected to negotiate the hybrid (green).
 *   2. https://github.com/ — GitHub edge does NOT advertise PQC;
 *      expected to fall back to classical X25519 (yellow).
 *
 * Requests run sequentially, not in parallel. The kx_tracker uses a
 * process-global atomic, so concurrent in-flight requests could race
 * each other — but since each response also carries its own
 * `X-Pqc-Negotiated-Group` header read at await time, sequencing keeps
 * the per-card display correct without depending on the atomic.
 *
 * iOS 26+ note: AppDelegate gates PqcURLProtocol off because Apple's
 * native URLSession already negotiates PQC. On that path the header is
 * absent; the UI falls back to an "unknown" state per card — the
 * handshake is still PQC, just not observable from the client side.
 */

import React, {useEffect, useState} from 'react';
import {
  ActivityIndicator,
  Platform,
  SafeAreaView,
  ScrollView,
  StatusBar,
  StyleSheet,
  Text,
  View,
} from 'react-native';

const EXPECTED_PQC_GROUP = 'X25519MLKEM768';
const KEX_HEADER = 'X-Pqc-Negotiated-Group';

type Target = {label: string; url: string};

const TARGETS: Target[] = [
  {label: 'Cloudflare (PQC edge)', url: 'https://pq.cloudflareresearch.com/'},
  {label: 'GitHub (classical edge)', url: 'https://github.com/'},
];

type CardResult =
  | {status: 'idle'}
  | {status: 'loading'}
  | {status: 'ok'; httpStatus: number; kex: string | null}
  | {status: 'error'; message: string};

export default function App(): React.JSX.Element {
  const [results, setResults] = useState<CardResult[]>(
    TARGETS.map(() => ({status: 'idle'})),
  );

  useEffect(() => {
    (async () => {
      // Sequential, not Promise.all — see file-level comment about
      // the kx_tracker race and per-request header reads.
      for (let i = 0; i < TARGETS.length; i++) {
        setResults(prev => {
          const next = [...prev];
          next[i] = {status: 'loading'};
          return next;
        });
        try {
          const resp = await fetch(TARGETS[i].url, {method: 'GET'});
          await resp.text(); // drain
          const kex =
            resp.headers.get(KEX_HEADER) ??
            resp.headers.get(KEX_HEADER.toLowerCase());
          setResults(prev => {
            const next = [...prev];
            next[i] = {status: 'ok', httpStatus: resp.status, kex};
            return next;
          });
        } catch (err: unknown) {
          setResults(prev => {
            const next = [...prev];
            next[i] = {
              status: 'error',
              message: err instanceof Error ? err.message : String(err),
            };
            return next;
          });
        }
      }
    })();
  }, []);

  return (
    <SafeAreaView style={styles.root}>
      <StatusBar barStyle="default" />
      <ScrollView contentContainerStyle={styles.scroll}>
        <Text style={styles.appTitle}>pqc-mobile-client</Text>
        <Text style={styles.appSubtitle}>Platform: {Platform.OS}</Text>

        {TARGETS.map((t, i) => (
          <ResultCard key={t.url} target={t} result={results[i]} />
        ))}
      </ScrollView>
    </SafeAreaView>
  );
}

function ResultCard({
  target,
  result,
}: {
  target: Target;
  result: CardResult;
}): React.JSX.Element {
  const isPqc = result.status === 'ok' && result.kex === EXPECTED_PQC_GROUP;

  return (
    <View style={styles.card}>
      <Text style={styles.cardTitle}>{target.label}</Text>
      <Text style={styles.cardUrl}>{target.url}</Text>

      {result.status === 'idle' && (
        <Text style={styles.muted}>queued…</Text>
      )}

      {result.status === 'loading' && (
        <View style={styles.spinner}>
          <ActivityIndicator size="small" />
          <Text style={styles.muted}>Performing TLS handshake…</Text>
        </View>
      )}

      {result.status === 'ok' && (
        <>
          <Text style={styles.label}>Negotiated KEX</Text>
          {result.kex === null ? (
            <>
              <Text style={[styles.value, styles.kexUnknown]}>unknown</Text>
              <Text style={styles.caption}>
                Native URLSession handled the request (iOS 26+).
              </Text>
            </>
          ) : (
            <>
              <Text
                style={[
                  styles.value,
                  isPqc ? styles.kexPqc : styles.kexClassical,
                ]}>
                {result.kex}
                {isPqc ? '  ✓ post-quantum' : '  (classical)'}
              </Text>
              <Text style={styles.caption}>
                {isPqc
                  ? 'Post-quantum hybrid handshake confirmed via the Rust core.'
                  : 'Classical handshake — PQC was offered but the server did not select it.'}
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
