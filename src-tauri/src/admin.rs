//! GET /admin: self-contained HTML+JS that consumes /events via EventSource.
//! A real-time debug dashboard reachable from an external browser; Tauri not required.

pub const ADMIN_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>OpenCapX Admin</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
  :root { --bg:#0b1020; --fg:#e6e8ef; --mut:#9aa3b2; --acc:#5dd3ff; --warn:#f59c6b; --err:#f87171; --ok:#4ade80; }
  * { box-sizing: border-box; }
  body { margin: 0; font: 13px/1.4 ui-monospace, Menlo, Consolas, monospace; background: var(--bg); color: var(--fg); }
  header { padding: 10px 16px; border-bottom: 1px solid #1f2944; display: flex; gap: 12px; align-items: center; }
  header h1 { font-size: 14px; margin: 0; font-weight: 500; }
  #status { font-size: 11px; padding: 2px 8px; border-radius: 999px; background: #1f2944; }
  #status.live { background: var(--ok); color: #0b1020; }
  #status.offline { background: var(--err); color: #0b1020; }
  main { padding: 12px 16px; }
  .row { display: flex; gap: 8px; margin-bottom: 6px; padding: 6px 8px; border-bottom: 1px solid #1f2944; }
  .row .kind { min-width: 200px; color: var(--acc); }
  .row .src { min-width: 160px; color: var(--mut); }
  .row .ts { color: var(--mut); margin-left: auto; }
  .row .payload { color: var(--fg); white-space: pre-wrap; word-break: break-word; }
  .toolbar { display: flex; gap: 8px; margin-bottom: 10px; }
  .toolbar button { background: #1f2944; color: var(--fg); border: 0; padding: 4px 10px; border-radius: 6px; cursor: pointer; font: inherit; }
  .toolbar input { background: #1f2944; color: var(--fg); border: 0; padding: 4px 8px; border-radius: 6px; font: inherit; min-width: 240px; }
  .toolbar button:hover { background: #2a3658; }
  .empty { color: var(--mut); padding: 20px; text-align: center; }
</style>
</head>
<body>
<header>
  <h1>OpenCapX · admin</h1>
  <span id="status">offline</span>
  <span style="color:var(--mut);font-size:11px">SSE: /events · control: POST /event</span>
</header>
<main>
  <div class="toolbar">
    <input id="filter" placeholder="filter: kind prefix (e.g. permission.)" />
    <button id="pause">Pause</button>
    <button id="clear">Clear</button>
    <button id="publish">Publish test event</button>
  </div>
  <div id="list"></div>
</main>
<script>
  const list = document.getElementById("list");
  const status = document.getElementById("status");
  const filterInput = document.getElementById("filter");
  const paused = { v: false };
  const buf = [];
  const MAX = 500;
  document.getElementById("pause").onclick = (e) => {
    paused.v = !paused.v;
    e.target.textContent = paused.v ? "Resume" : "Pause";
  };
  document.getElementById("clear").onclick = () => { buf.length = 0; render(); };
  document.getElementById("publish").onclick = async () => {
    try {
      await fetch("/event", {
        method: "POST",
        headers: {"Content-Type":"application/json"},
        body: JSON.stringify({agent:"admin.test", text:"hello from admin"})
      });
    } catch (e) { console.error(e); }
  };
  filterInput.oninput = render;
  function fmt(ts) {
    if (!ts) return "";
    const d = new Date(ts * 1000);
    return d.toISOString().replace("T"," ").slice(0,19);
  }
  function render() {
    const f = filterInput.value.trim();
    const view = f ? buf.filter(e => e.kind.startsWith(f)) : buf;
    if (view.length === 0) {
      list.innerHTML = '<div class="empty">no events' + (f ? ' match filter' : ' yet') + '</div>';
      return;
    }
    list.innerHTML = view.slice(-MAX).map(e =>
      `<div class="row"><span class="kind">${esc(e.kind)}</span><span class="src">${esc(e.source||"")}</span><span class="ts">${esc(fmt(e.timestamp))}</span><span class="payload">${esc(JSON.stringify(e.payload ?? null))}</span></div>`
    ).join("");
  }
  function esc(s) { return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;"); }
  const es = new EventSource("/events");
  es.onopen = () => { status.textContent = "live"; status.className = "live"; };
  es.onerror = () => { status.textContent = "offline"; status.className = "offline"; };
  es.onmessage = (m) => {
    if (paused.v) return;
    try {
      const ev = JSON.parse(m.data);
      buf.push(ev);
      if (buf.length > 2000) buf.splice(0, buf.length - 2000);
      render();
    } catch {}
  };
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admin_html_is_self_contained() {
        assert!(ADMIN_HTML.starts_with("<!doctype html>"));
        assert!(ADMIN_HTML.contains("EventSource"));
        assert!(ADMIN_HTML.contains("/events"));
        assert!(ADMIN_HTML.contains("/event"));
        // Must include the filter/pause/clear controls
        assert!(ADMIN_HTML.contains("filter"));
        assert!(ADMIN_HTML.contains("pause") || ADMIN_HTML.contains("Pause"));
        assert!(ADMIN_HTML.contains("clear") || ADMIN_HTML.contains("Clear"));
    }
}