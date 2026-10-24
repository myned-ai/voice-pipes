// The production image's nginx config. nginx drops the server-level
// add_header set from any location that has an add_header of its own, so
// every location must include the security-header snippet itself; and the
// entry document must not be cached, or a deploy keeps serving the previous
// release's bundle names. The container check (`curl -I`) is the live
// version of this; this one runs with the unit suite.
import nginxConf from '../nginx.conf?raw'
import securityHeaders from '../security-headers.conf?raw'
import dockerfile from '../Dockerfile?raw'

const SNIPPET = '/etc/nginx/snippets/security-headers.conf'

function locations(conf: string): { name: string; body: string }[] {
  const out: { name: string; body: string }[] = []
  const re = /location\s+([^{]+)\{([^}]*)\}/g
  let m: RegExpExecArray | null
  while ((m = re.exec(conf)) !== null) {
    out.push({ name: m[1].trim(), body: m[2] })
  }
  return out
}

function directives(conf: string): string[] {
  return conf
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('#'))
}

describe('admin frontend nginx config', () => {
  it('the snippet sets the security headers on every response', () => {
    const lines = directives(securityHeaders)
    for (const header of [
      'X-Content-Type-Options "nosniff"',
      'X-Frame-Options "DENY"',
      `Content-Security-Policy "frame-ancestors 'none'"`,
      'Referrer-Policy',
    ]) {
      const line = lines.find((l) => l.startsWith(`add_header ${header}`))
      expect(line, header).toBeDefined()
      expect(line, header).toMatch(/ always;$/)
    }
  })

  it('every location includes the snippet', () => {
    const locs = locations(nginxConf)
    expect(locs.length).toBeGreaterThan(0)
    for (const loc of locs) {
      expect(loc.body, loc.name).toContain(`include ${SNIPPET};`)
    }
  })

  it('the entry document is revalidated, the hashed assets are immutable', () => {
    const locs = Object.fromEntries(locations(nginxConf).map((l) => [l.name, l.body]))
    expect(locs['= /index.html']).toContain('add_header Cache-Control "no-cache";')
    expect(locs['/assets/']).toContain('immutable')
    // The SPA fallback ends at /index.html, so it takes those headers.
    expect(locs['/']).toMatch(/try_files [^;]* \/index\.html;/)
  })

  it('the image copies the snippet to the path the config includes', () => {
    expect(dockerfile).toContain(`COPY security-headers.conf ${SNIPPET}`)
  })
})
