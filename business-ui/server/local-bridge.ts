import http from 'node:http';
import https from 'node:https';
import type { Plugin } from 'vite';
import { handleBusiness } from './business';

// >>> amux-endpoint-guard (rule: scripts/lib/amux_endpoint.py; every copy identical, test-enforced)
function amuxEndpointGuard(url: unknown, client: string): unknown {
  const u = String(url || '').trim().toLowerCase();
  const a = u.replace(/^[a-z][a-z0-9+.-]*:\/\//, '').split(/[/?#]/)[0].replace(/^.*@/, '').replace(/^\[.*\]/, '');
  const p = a.includes(':') ? a.slice(a.lastIndexOf(':') + 1) : '';
  const live = new Set([8824, 8822]);
  const mod = (m: string): any => (process as any).getBuiltinModule(m);
  const e = process.env;
  for (const h of [mod('path').join(mod('os').userInfo().homedir, '.amux'), e.AMUX_HOME, e.CC_HOME, e.HOME && `${e.HOME}/.amux`]) {
    try {
      const ep = JSON.parse(mod('fs').readFileSync(`${h}/endpoint.json`, 'utf8'));
      for (const n of [ep.canonical_port, ep.legacy_port, ...(ep.retired_ports || [])]) if (Number.isInteger(n)) live.add(n);
    } catch {}
  }
  if (process.env.AMUX_ISOLATED_RUN === '1' && (!u || (/^[0-9]+$/.test(p) && live.has(Number(p))))) {
    process.stderr.write(`[amux] verdict=isolated_live_endpoint_refused client=${client} from=${url || '<unset>'} reason=live_port to=http://127.0.0.1:9 measured=true n_considered=1\n`);
    return 'http://127.0.0.1:9';
  }
  return url;
}
// <<< amux-endpoint-guard
// Development transport only. Production can reverse-proxy /api/business to the same adapter,
// or use the Worker route with an authenticated, externally reachable Amux URL.
export function businessBridge(): Plugin {
  return {
    name: 'amux-business-bridge',
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (!req.url?.startsWith('/api/business/')) return next();
        const host = req.headers.host || '';
        if (!/^(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/.test(host)) {
          res.writeHead(403);
          res.end('Local access only');
          return;
        }
        const origin = req.headers.origin;
        if (
          origin &&
          origin !== `http://${host}` &&
          origin !== `https://${host}`
        ) {
          res.writeHead(403);
          res.end('Same-origin access required');
          return;
        }
        if (
          req.method !== 'GET' &&
          req.headers['content-type']?.split(';')[0] !== 'application/json'
        ) {
          res.writeHead(415);
          res.end('JSON required');
          return;
        }
        const upstream = new URL(
          String(amuxEndpointGuard(process.env.AMUX_SERVER_URL || 'https://localhost:8824', 'business-ui-bridge')),
        );
        if (!['http:', 'https:'].includes(upstream.protocol)) {
          res.writeHead(503);
          res.end('Invalid Amux server URL');
          return;
        }
        const chunks: Buffer[] = [];
        let size = 0;
        for await (const chunk of req) {
          size += chunk.length;
          if (size > 100_000) {
            res.writeHead(413);
            res.end('Request too large');
            return;
          }
          chunks.push(chunk);
        }
        const request = new Request(`http://${host}${req.url}`, {
          method: req.method,
          headers: { 'Content-Type': 'application/json' },
          ...(req.method !== 'GET' && req.method !== 'HEAD'
            ? { body: Buffer.concat(chunks).toString() }
            : {}),
        });
        const result = await handleBusiness(
          request,
          async (path, init = {}) =>
            new Promise<Response>((resolve, reject) => {
              const target = new URL(path, upstream);
              const headers: Record<string, string> = {
                Accept: 'application/json',
                ...(init.headers as Record<string, string>),
              };
              if (
                process.env.AMUX_AUTH_TOKEN &&
                process.env.AMUX_AUTH_TOKEN !== 'none'
              )
                headers.Authorization = 'Bearer ' + process.env.AMUX_AUTH_TOKEN;
              // Preserve explicit login from the existing owner session; never expose it to client JavaScript.
              if (req.headers.authorization)
                headers.Authorization = req.headers.authorization;
              if (req.headers.cookie) headers.Cookie = req.headers.cookie;
              const local = ['localhost', '127.0.0.1', '[::1]'].includes(
                target.hostname,
              );
              const upstreamRequest = (
                target.protocol === 'https:' ? https : http
              ).request(
                target,
                {
                  method: init.method || 'GET',
                  headers,
                  ...(target.protocol === 'https:'
                    ? { rejectUnauthorized: !local }
                    : {}),
                },
                (response) => {
                  const data: Buffer[] = [];
                  response.on('data', (chunk) => data.push(chunk));
                  response.on('end', () =>
                    resolve(
                      new Response(Buffer.concat(data).toString(), {
                        status: response.statusCode || 502,
                        headers: { 'Content-Type': 'application/json' },
                      }),
                    ),
                  );
                },
              );
              upstreamRequest.setTimeout(15000, () =>
                upstreamRequest.destroy(new Error('upstream_timeout')),
              );
              upstreamRequest.on('error', reject);
              if (init.body) upstreamRequest.write(init.body);
              upstreamRequest.end();
            }),
        );
        res.writeHead(result.status, Object.fromEntries(result.headers));
        res.end(await result.text());
      });
    },
  };
}
