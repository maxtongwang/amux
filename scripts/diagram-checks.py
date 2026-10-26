#!/usr/bin/env python3
"""Repo-side checks that sit beside the vendored check-diagrams.py.

phone      Write (--write) or verify the phone view of each diagram. The phone view is
           a stacked list of boxes and arrows derived from the page's own SVG, shown
           below 720 px so the figure is readable without sideways scrolling.
inventory  Verify docs/diagrams/ARCHITECTURE_PATHS: every tracked file is either an
           architecture path or excluded with a reason, every repo path a diagram
           cites is an architecture path, and in a scratch Git fixture a one-line
           change to one file per glob fails check-diagrams.py while a change to an
           excluded file passes.
all        Both of the above (default).

Exit status: 0 pass, 1 check failed, 2 invalid input. A JSON report goes to stdout.
"""
import argparse
import fnmatch
import json
import math
import re
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

DIAGRAMS = Path('docs/diagrams')
VIEWS = ('topology.html', 'runtime-sequence.html', 'data-flow.html')
SVG_NS = '{http://www.w3.org/2000/svg}'
EXCLUDE_PREFIX = '# exclude:'
# Named files the fixture must also probe, e.g. a past miss: '# probe: src/lock.py'.
PROBE_PREFIX = '# probe:'
REASON_SEP = ' — '
# Arrow heads stop short of a box edge by the marker length; starts sit on the edge.
START_SLACK_PX, END_SLACK_PX = 6, 14
# A label further than this from every arrow is a drawing error, not a caption.
LABEL_MAX_PX = 40
# Average advance of the 8-9 px monospace caption fonts.
CHAR_PX = 5.4
PHONE_BREAK_PX = 720
LIFELINE_STROKE = '#bfc0c0'
BEGIN, END = '<!--phone-view-->', '<!--/phone-view-->'
CSS_MARK = '/*phone-view*/'
PHONE_CSS = (CSS_MARK + '.phone{display:none}.phone h2{font:400 1.25rem \'Instrument Serif\',serif;margin:20px 0 4px}'
             '.phone ol{list-style:none;margin:0;padding:0}.phone .node{margin:10px 0;padding:10px 12px;border:1px solid #2d3142;border-radius:6px;background:#fff}'
             '.phone .hot{border-color:#bf4520;background:#f0e7e3}.phone b{display:block;font:600 14px \'Geist\',sans-serif}'
             '.phone .sub{display:block;font:400 11px \'Geist Mono\',monospace;color:#5a6580;overflow-wrap:anywhere}'
             '.phone .edge{margin:6px 0 0;font:400 12px \'Geist\',sans-serif;overflow-wrap:anywhere}'
             '.phone .lab{font:500 10px \'Geist Mono\',monospace;letter-spacing:.06em;color:#4f5d75}'
             f'@media(max-width:{PHONE_BREAK_PX}px){{.phone{{display:block}}svg{{min-width:0}}.scroll{{overflow:visible}}}}')
COMMENT_LINE = {'.py': '# {}', '.sh': '# {}', '.toml': '# {}', '.yml': '# {}', '.yaml': '# {}', '': '# {}',
                '.rs': '// {}', '.js': '// {}', '.cjs': '// {}', '.mjs': '// {}', '.ts': '// {}',
                '.html': '<!-- {} -->', '.md': '<!-- {} -->', '.css': '/* {} */', '.sql': '-- {}'}
PROBE = 'diagram inventory probe'


class Invalid(Exception):
    """Input the checks cannot evaluate (exit 2)."""


# ---------- SVG model ----------

def parse_svg(page):
    start, stop = page.find('<svg'), page.find('</svg>')
    if start < 0 or stop < 0:
        raise Invalid('no inline <svg> element')
    return ET.fromstring(page[start:stop + 6])


def num(el, key):
    return float(el.get(key, '0'))


def path_points(d):
    """Endpoints and control points of an M/L/Q path, in drawing order."""
    vals = [float(v) for v in re.findall(r'-?\d+(?:\.\d+)?', d)]
    if re.search(r'[^MLQ\d\s,.\-]', d) or len(vals) % 2:
        raise Invalid(f'unsupported path data: {d!r}')
    return list(zip(vals[::2], vals[1::2]))


def seg_dist(p, a, b):
    (px, py), (ax, ay), (bx, by) = p, a, b
    dx, dy = bx - ax, by - ay
    t = 0 if dx == dy == 0 else max(0, min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - ax - t * dx, py - ay - t * dy)


def poly_dist(p, pts):
    return min(seg_dist(p, a, b) for a, b in zip(pts, pts[1:]))


def model(page):
    """Boxes, arrows with captions, and whether the figure is a sequence."""
    svg = parse_svg(page)
    els = list(svg.iter())
    tag = lambda el: el.tag.replace(SVG_NS, '')
    legend_y = min((num(l, 'y1') for l in els if tag(l) == 'line' and l.get('stroke') == LIFELINE_STROKE
                    and num(l, 'y1') == num(l, 'y2') and abs(num(l, 'x2') - num(l, 'x1')) > 1000), default=math.inf)
    boxes = []
    for r in els:
        if tag(r) == 'rect' and r.get('stroke') and num(r, 'width') >= 100:
            boxes.append({'x': num(r, 'x'), 'y': num(r, 'y'), 'w': num(r, 'width'), 'h': num(r, 'height'),
                          'hot': r.get('fill', '').lower() != '#ffffff', 'name': '', 'subs': []})
    inside = lambda b, x, y, s=0: b['x'] - s <= x <= b['x'] + b['w'] + s and b['y'] - s <= y <= b['y'] + b['h'] + s
    loose, legend = [], []
    for t in els:
        if tag(t) != 'text' or t.get('class') not in ('name', 'sub', 'label'):
            continue
        x, y, text = num(t, 'x'), num(t, 'y'), ''.join(t.itertext()).strip()
        if y > legend_y:
            legend.append(text)
            continue
        box = next((b for b in boxes if inside(b, x, y)), None)
        if box and t.get('class') == 'name':
            box['name'] = text
        elif box and t.get('class') == 'sub':
            box['subs'].append(text)
        elif box:
            raise Invalid(f'label {text!r} sits inside a box')
        else:
            loose.append((t.get('class'), x, y, text, t.get('text-anchor', 'start')))
    for b in boxes:
        if not b['name']:
            raise Invalid(f'box at {b["x"]:.0f},{b["y"]:.0f} has no name')
    lifelines = sorted(num(l, 'x1') for l in els if tag(l) == 'line' and l.get('stroke') == LIFELINE_STROKE
                       and num(l, 'x1') == num(l, 'x2') and num(l, 'y2') - num(l, 'y1') > 100)
    arrows = []
    for el in els:
        if not el.get('marker-end') or tag(el) == 'marker':
            continue
        pts = path_points(el.get('d', '')) if tag(el) == 'path' else [(num(el, 'x1'), num(el, 'y1')), (num(el, 'x2'), num(el, 'y2'))]
        arrows.append({'pts': pts, 'dashed': bool(el.get('stroke-dasharray')), 'label': [], 'detail': []})
    if not arrows:
        raise Invalid('no arrows')
    def dist(arrow, x, y, text, anchor):
        # Measure from points along the caption's estimated width, a little above its baseline.
        width = len(text) * CHAR_PX
        left = x - width / 2 if anchor == 'middle' else x
        return min(poly_dist((left + width * i / 4, y - 4), arrow['pts']) for i in range(5))
    # Arrow labels pair one to one, closest pairs first; details join their nearest arrow.
    labels = [c for c in loose if c[0] == 'label']
    pairs = sorted((dist(a, *c[1:]), i, j) for i, c in enumerate(labels) for j, a in enumerate(arrows))
    used_l, used_a = set(), set()
    for d, i, j in pairs:
        if i in used_l or j in used_a or d > LABEL_MAX_PX:
            continue
        used_l.add(i); used_a.add(j)
        arrows[j]['label'].append(labels[i][3])
    for i, c in enumerate(labels):
        if i not in used_l:
            raise Invalid(f'label {c[3]!r} is not next to a free arrow')
    for c in loose:
        if c[0] != 'label':
            a = min(arrows, key=lambda a: dist(a, *c[1:]))
            if dist(a, *c[1:]) > LABEL_MAX_PX:
                raise Invalid(f'caption {c[3]!r} is not next to any arrow')
            a['detail'].append(c[3])
    seq = bool(lifelines)
    if seq:
        actors = sorted((b for b in boxes if any(abs(b['x'] + b['w'] / 2 - l) < 2 for l in lifelines)), key=lambda b: b['x'])
        if len(actors) != len(lifelines):
            raise Invalid('sequence lifelines do not match actor boxes')
        near = lambda x: min(actors, key=lambda b: abs(b['x'] + b['w'] / 2 - x))
        for a in arrows:
            a['src'], a['dst'] = near(a['pts'][0][0]), near(a['pts'][-1][0])
        arrows.sort(key=lambda a: a['pts'][0][1])
        boxes = actors
    else:
        for a in arrows:
            (sx, sy), (ex, ey) = a['pts'][0], a['pts'][-1]
            src = [b for b in boxes if inside(b, sx, sy, START_SLACK_PX)]
            dst = [b for b in boxes if inside(b, ex, ey, END_SLACK_PX)]
            if len(src) != 1 or len(dst) != 1:
                raise Invalid(f'arrow {a["pts"][0]}->{a["pts"][-1]} does not join exactly two boxes')
            a['src'], a['dst'] = src[0], dst[0]
        boxes.sort(key=lambda b: (b['y'], b['x']))
    for a in arrows:
        if not a['label']:
            raise Invalid(f'arrow from {a["src"]["name"]!r} to {a["dst"]["name"]!r} has no label')
    return boxes, arrows, seq, ' '.join(legend)


# ---------- phone view ----------

def esc(s):
    return s.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')


def card(b, extra=''):
    subs = ''.join(f'<span class="sub">{esc(s)}</span>' for s in b['subs'])
    return f'<li class="node{" hot" if b["hot"] else ""}"><b>{esc(b["name"])}</b>{subs}{extra}</li>'


def edge_text(a, other):
    kind = ' (dashed)' if a['dashed'] else ''
    detail = ''.join(f'<span class="sub">{esc(d)}</span>' for d in a['detail'])
    return f'<span class="lab">{esc(" · ".join(a["label"]))}</span>{kind} {other}{detail}'


def phone_view(page):
    boxes, arrows, seq, legend = model(page)
    key = f'<p class="sub">{esc(legend)}</p>' if legend else ''
    if seq:
        steps = ''.join(f'<li class="edge">{edge_text(a, esc(a["src"]["name"]) + " → " + esc(a["dst"]["name"]))}</li>' for a in arrows)
        body = (f'{key}<h2>Participants</h2><ol>{"".join(card(b) for b in boxes)}</ol>'
                f'<h2>Steps, top to bottom</h2><ol class="node">{steps}</ol>')
    else:
        items = []
        for b in boxes:
            outs = ''.join(f'<div class="edge">→ {edge_text(a, esc(a["dst"]["name"]))}</div>' for a in arrows if a['src'] is b)
            items.append(card(b, outs))
        body = f'{key}<h2>Boxes and their outgoing arrows</h2><ol>{"".join(items)}</ol>'
    return f'{BEGIN}<section class="phone" aria-label="Phone view of the figure">{body}</section>{END}'


def with_phone(page):
    page = re.sub(re.escape(BEGIN) + '.*?' + re.escape(END), '', page, flags=re.S)
    page = re.sub(re.escape(CSS_MARK) + r'.*?(?=</style>)', '', page, flags=re.S)
    if '</style>' not in page or '</svg></div>' not in page:
        raise Invalid('page lacks </style> or the </svg></div> figure wrapper')
    page = page.replace('</style>', PHONE_CSS + '\n</style>', 1)
    return page.replace('</svg></div>', '</svg></div>' + phone_view(page), 1)


def check_phone(repo, write):
    errors, changed = [], []
    for name in VIEWS:
        path = repo / DIAGRAMS / name
        try:
            page = path.read_text()
            new = with_phone(page)
        except (OSError, Invalid, ET.ParseError) as exc:
            raise Invalid(f'{name}: {exc}') from exc
        if new != page:
            changed.append(name)
            if write:
                path.write_text(new)
            else:
                errors.append(f'{name}: phone view is missing or stale; run scripts/diagram-checks.py phone --write')
    return {'phone_changed': changed, 'phone_errors': errors}


# ---------- inventory ----------

def git(repo, *args, check=True):
    res = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, text=True)
    if check and res.returncode:
        raise Invalid(f'git {" ".join(args)}: {res.stderr.strip()}')
    return res


def read_inventory(repo):
    includes, excludes, probes = [], [], []
    for n, raw in enumerate((repo / DIAGRAMS / 'ARCHITECTURE_PATHS').read_text().splitlines(), 1):
        line = raw.strip()
        if line.startswith(EXCLUDE_PREFIX):
            glob, sep, reason = line[len(EXCLUDE_PREFIX):].strip().partition(REASON_SEP)
            if not sep or not reason.strip() or not glob.strip():
                raise Invalid(f'ARCHITECTURE_PATHS:{n}: exclusion needs "<glob>{REASON_SEP}<reason>"')
            excludes.append(glob.strip())
        elif line.startswith(PROBE_PREFIX):
            probes.append(line[len(PROBE_PREFIX):].split(REASON_SEP)[0].strip())
        elif line and not line.startswith('#'):
            includes.append(line)
    return includes, excludes, probes


def cited_paths(repo, files):
    """Tracked files named by a box subtitle or caption, e.g. 'src/cli.py' or 'tools/*/render.cjs'."""
    cited = {}
    for name in VIEWS:
        svg = parse_svg((repo / DIAGRAMS / name).read_text())
        for t in svg.iter(SVG_NS + 'text'):
            for token in re.split(r'\s+·\s+|\s+', ''.join(t.itertext())):
                token = token.strip('(),')
                if '/' not in token or token.startswith(('~', '/', 'http')):
                    continue
                pat = token.rstrip('/')
                # A file or file glob is a code citation; a bare folder names a store of content.
                hits = [f for f in files if fnmatch.fnmatchcase(f, pat)]
                for f in hits:
                    cited.setdefault(f, f'{name}: {token}')
    return cited


def probe(path, text):
    ext = Path(path).suffix
    line = COMMENT_LINE.get(ext, '{}').format(PROBE) if ext != '.json' else ''
    return text + ('' if text.endswith('\n') or not text else '\n') + line + '\n'


def fixture_run(repo, checker, samples, control):
    """Commit the diagram set plus samples, then change one sample per commit."""
    results = {}
    with tempfile.TemporaryDirectory(prefix='diagram-inventory-') as tmp:
        fx = Path(tmp)
        git(fx, 'init', '-q')
        git(fx, 'config', 'user.email', 'probe@invalid')
        git(fx, 'config', 'user.name', 'probe')
        keep = {str(DIAGRAMS / n) for n in (*VIEWS, 'README.md', 'ARCHITECTURE_PATHS')} | {'README.md'}
        for f in sorted(keep | set(samples) | ({control} if control else set())):
            dest = fx / f
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes((repo / f).read_bytes())
        git(fx, 'add', '-A')
        git(fx, 'commit', '-q', '-m', 'base')
        base = git(fx, 'rev-parse', 'HEAD').stdout.strip()
        for f, want in [(s, 1) for s in samples] + ([(control, 0)] if control else []):
            p = fx / f
            p.write_text(probe(f, p.read_text(errors='replace')))
            git(fx, 'commit', '-q', '-am', f'probe {f}')
            res = subprocess.run([sys.executable, str(checker), '--repo', str(fx), '--base', base], capture_output=True, text=True)
            results[f] = {'want': want, 'got': res.returncode}
            git(fx, 'reset', '-q', '--hard', base)
    return results


def check_inventory(repo):
    includes, excludes, probes = read_inventory(repo)
    if not includes:
        raise Invalid('ARCHITECTURE_PATHS has no architecture globs')
    # -z keeps non-ASCII and quoted names verbatim.
    files = [f for f in git(repo, 'ls-files', '-z').stdout.split('\0') if f]
    match = lambda pats, f: any(fnmatch.fnmatchcase(f, p) for p in pats)
    errors = []
    unclassified = [f for f in files if not match(includes, f) and not match(excludes, f)]
    errors += [f'unclassified tracked file: {f}' for f in unclassified]
    errors += [f'architecture glob matches no tracked file: {p}' for p in includes if not any(fnmatch.fnmatchcase(f, p) for f in files)]
    errors += [f'exclusion matches no tracked file: {p}' for p in excludes if not any(fnmatch.fnmatchcase(f, p) for f in files)]
    cited = cited_paths(repo, files)
    errors += [f'diagram cites {f} ({where}) but it is not an architecture path' for f, where in sorted(cited.items()) if not match(includes, f)]
    errors += [f'probe is not a tracked architecture path: {f}' for f in probes if f not in files or not match(includes, f)]
    samples = sorted({next(f for f in sorted(files) if fnmatch.fnmatchcase(f, p)) for p in includes if any(fnmatch.fnmatchcase(f, p) for f in files)}
                     | {f for f in probes if f in files and match(includes, f)})
    control = next((f for f in sorted(files) if not match(includes, f) and not f.startswith(str(DIAGRAMS) + '/')), None)
    results = fixture_run(repo, repo / 'scripts' / 'check-diagrams.py', samples, control)
    errors += [f'fixture: change to {f} exited {r["got"]}, expected {r["want"]}' for f, r in results.items() if r['got'] != r['want']]
    return {'tracked_files': len(files), 'architecture_files': sum(match(includes, f) for f in files),
            'excluded_files': sum(not match(includes, f) for f in files), 'cited_files': len(cited),
            'fixture': results, 'inventory_errors': errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('command', nargs='?', default='all', choices=('all', 'phone', 'inventory'))
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--write', action='store_true', help='phone: rewrite stale phone views instead of failing')
    args = parser.parse_args()
    report = {'status': 'pass'}
    try:
        if args.command in ('all', 'phone'):
            report.update(check_phone(args.repo, args.write))
        if args.command in ('all', 'inventory'):
            report.update(check_inventory(args.repo))
    except (Invalid, OSError, ET.ParseError) as exc:
        print(json.dumps({'status': 'error', 'errors': [str(exc)]}, indent=2))
        return 2
    if report.get('phone_errors') or report.get('inventory_errors'):
        report['status'] = 'fail'
    print(json.dumps(report, indent=2))
    return 0 if report['status'] == 'pass' else 1


if __name__ == '__main__':
    sys.exit(main())
