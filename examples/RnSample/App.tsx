/**
 * PQC Mobile Client — RN integration sample.
 *
 * On launch, makes an HTTPS request to pq.cloudflareresearch.com via
 * fetch(). The Cloudflare research endpoint echoes the negotiated TLS
 * key-exchange group back in the response body — look for
 * "kex = X25519MLKEM768" to confirm the post-quantum hybrid handshake.
 *
 * On Android: fetch() routes through OkHttp → PqcInterceptor →
 *             pqc_client (see android/app/src/main/java/com/rnsample/MainApplication.kt).
 * On iOS:     fetch() routes through NSURLSession → PqcURLProtocol →
 *             pqc_client (see ios/RnSample/AppDelegate.mm).
 *
 * On iOS 26+ the URLProtocol path is skipped (Apple's native URLSession
 * already negotiates X25519MLKEM768) — Cloudflare's endpoint reports the
 * same group either way.
 */

import React, {useEffect, useState} from 'react';
import {
  ActivityIndicator,
  Platform,
  SafeAreaView,
  StatusBar,
  StyleSheet,
  Text,
  View,
} from 'react-native';

const TEST_URL = 'https://pq.cloudflareresearch.com/';

type Result =
  | {status: 'loading'}
  | {status: 'ok'; httpStatus: number; bodyExcerpt: string}
  | {status: 'error'; message: string};

export default function App(): React.JSX.Element {
  const [result, setResult] = useState<Result>({status: 'loading'});

  useEffect(() => {
    (async () => {
      try {
        const resp = await fetch(TEST_URL, {
          method: 'GET',
          headers: {Accept: 'text/plain'},
        });
        const text = await resp.text();
        setResult({
          status: 'ok',
          httpStatus: resp.status,
          bodyExcerpt: text.slice(0, 240),
        });
      } catch (err: unknown) {
        setResult({
          status: 'error',
          message: err instanceof Error ? err.message : String(err),
        });
      }
    })();
  }, []);

  return (
    <SafeAreaView style={styles.root}>
      <StatusBar barStyle="default" />
      <View style={styles.card}>
        <Text style={styles.title}>pqc-mobile-client</Text>
        <Text style={styles.subtitle}>{TEST_URL}</Text>
        <Text style={styles.platform}>Platform: {Platform.OS}</Text>

        {result.status === 'loading' && (
          <View style={styles.spinner}>
            <ActivityIndicator size="large" />
            <Text style={styles.muted}>Performing TLS handshake…</Text>
          </View>
        )}

        {result.status === 'ok' && (
          <>
            <Text style={styles.label}>HTTP status</Text>
            <Text style={styles.value}>{result.httpStatus}</Text>
            <Text style={styles.label}>Response body (excerpt)</Text>
            <Text style={styles.body}>{result.bodyExcerpt}</Text>
            <Text style={styles.hint}>
              Look for "kex = X25519MLKEM768" — that's the post-quantum hybrid
              group negotiated by the Rust core.
            </Text>
          </>
        )}

        {result.status === 'error' && (
          <>
            <Text style={styles.label}>Error</Text>
            <Text style={styles.error}>{result.message}</Text>
          </>
        )}
      </View>
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  root: {flex: 1, backgroundColor: '#0b0d11', justifyContent: 'center', padding: 16},
  card: {backgroundColor: '#161a22', borderRadius: 14, padding: 20},
  title: {color: '#e7eaf0', fontSize: 22, fontWeight: '600'},
  subtitle: {color: '#7d8595', fontSize: 13, marginTop: 4, marginBottom: 14},
  platform: {color: '#5d97f7', fontSize: 13, marginBottom: 16},
  label: {color: '#7d8595', fontSize: 12, marginTop: 12, textTransform: 'uppercase'},
  value: {
    color: '#e7eaf0',
    fontSize: 18,
    fontFamily: Platform.select({ios: 'Menlo', default: 'monospace'}),
  },
  body: {
    color: '#c8cdd6',
    fontSize: 13,
    fontFamily: Platform.select({ios: 'Menlo', default: 'monospace'}),
    marginTop: 4,
  },
  error: {color: '#ff6f6f', fontSize: 13, marginTop: 4},
  muted: {color: '#7d8595', fontSize: 13, marginTop: 8, textAlign: 'center'},
  spinner: {marginVertical: 24, alignItems: 'center'},
  hint: {color: '#7d8595', fontSize: 12, marginTop: 16, fontStyle: 'italic'},
});
