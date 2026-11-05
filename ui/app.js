'use strict';

// The post-sync reveal: every clip floats from where it sat before the sync
// to its synced position together, like an accordion opening. Purely
// presentational (the sync is already done). Click or any key skips it.
const FOCUS_MS        = 900;  // easing the view to a clip (footer counters, Show them)
const FOCUS_CLIP_SHARE = 0.07; // …zoomed so the clip fills about this much of the view
const REMOVE_FADE_MS  = 300;  // deleted clips fade out…
const REMOVE_PAUSE_MS = 250;  // …then a beat before zooming out
const REVIEW_HOLD_MS  = 450;  // Keep/Remove: how long the green/red shows before zooming out
const REVIEW_ZOOM_MS  = 500;  // zoom out to the whole timeline, and in to the next clip
const REVIEW_PAUSE_MS = 250;  // whole timeline shown before going to the next clip
const SYNC_BASE_MS     = 700;   // 0 = off
const SYNC_CLIP_MS     = 700;   // the glide into place
const WAVE_HEIGHT      = 0.55;  // tallest bar, as a share of the clip's height
const SWEEP_MS         = 900;   // confirm sweep across the screen after clips land
const SYNC_EASE        = 'cubic-bezier(.65,0,.35,1)';  // smooth in and out

const DEFAULT_PREFS = { fmt: null, strip: false, drop: false };  // no editor until the user picks one

// Export choices are remembered between launches, in a settings file the
// app keeps on disk (browser storage as a fallback).
const prefs = {
  _key: 'shotsynq.export',
  _saved: null,
  async load() {
    try {
      const T = window.__TAURI__;
      if (T) this._saved = await T.core.invoke('get_prefs');
    } catch (e) { /* fall back below */ }
    if (!this._saved || !Object.keys(this._saved).length) {
      try { this._saved = JSON.parse(localStorage.getItem(this._key) || '{}'); } catch (e) { this._saved = {}; }
    }
  },
  get() { return Object.assign({}, DEFAULT_PREFS, this._saved || {}); },
  set(v) {
    this._saved = Object.assign({}, v);
    try { localStorage.setItem(this._key, JSON.stringify(v)); } catch (e) { /* ignore */ }
    try { const T = window.__TAURI__; if (T) T.core.invoke('set_prefs', { prefs: v }); } catch (e) { /* ignore */ }
  },
};

const app = {
  _state:        null,
  _filter:       null,  // null | 'synced' | 'review' | 'unsynced'
  _selected:     null,  // the clip clicked last: { trackIndex, clipIndex, tray }
  _selection:    [],    // every selected clip, as "track|clip" keys (Shift/Ctrl+click adds)
  _pps:          100,   // pixels per second
  _userZoomed:   false, // true once user has manually zoomed

  // ── State update (pushed from Rust) ──────────────────────────────────────

  // New state from Rust. Many can arrive at once while files load; draw at
  // most once per frame, with the newest.
  update(state) {
    this._pendingState = state;
    if (this._raf) return;
    this._raf = requestAnimationFrame(() => {
      this._raf = null;
      const s = this._pendingState;
      this._pendingState = null;
      if (s) this._apply(s);
    });
  },

  _apply(state) {
    const prev = this._state;
    // (a quick sync can finish within one frame, so the syncing state may never be drawn)
    const justSynced = prev && !state.buttons.cancel && (prev.buttons.cancel || !prev.timeline.synced) &&
      state.timeline.synced && ['success', 'warning'].includes(state.status.type);
    this._finishAnim();
    const before = justSynced && this._animEnabled() ? this._capturePositions() : null;
    // While loading (before any sync), clips glide to their new places
    const smooth = !justSynced && prev && !state.timeline.synced && !prev.timeline.synced && this._animEnabled()
      ? this._capturePlaces() : null;
    this._state = state;
    if (before) this._holdPanels(true);  // before drawing, so they never flash up
    this._render();
    // A move just made: say what happened (the state's own status is blank)
    if (this._note) { this.setStatus(this._note, 'default'); this._note = null; }
    if (before) this._animateSync(before, state.status);
    if (smooth) this._smoothLayout(smooth);
  },

  // Where every clip and track label is on screen, by name
  _capturePlaces() {
    const places = new Map();
    document.querySelectorAll('#tracks-inner .clip').forEach(el => {
      const r = el.getBoundingClientRect();
      places.set('c:' + el.dataset.key, { x: r.left, y: r.top });
    });
    document.querySelectorAll('#sidebar-tracks .track-label').forEach(el => {
      const r = el.getBoundingClientRect();
      places.set('t:' + el.dataset.name, { x: r.left, y: r.top });
    });
    return places;
  },

  // Slide anything that moved from its old place; fade in anything new
  _smoothLayout(before) {
    const move = (el, key) => {
      const old = before.get(key);
      if (!old) { el.classList.add('appear'); return; }
      const r = el.getBoundingClientRect();
      const dx = old.x - r.left, dy = old.y - r.top;
      if (Math.abs(dx) + Math.abs(dy) < 1) return;
      el.style.transition = 'none';
      el.style.transform = `translate(${dx}px, ${dy}px)`;
      void el.offsetWidth;
      el.style.transition = 'transform 240ms cubic-bezier(.2,.7,.2,1)';
      el.style.transform = '';
      setTimeout(() => { el.style.transition = ''; }, 260);
    };
    document.querySelectorAll('#tracks-inner .clip').forEach(el => move(el, 'c:' + el.dataset.key));
    document.querySelectorAll('#sidebar-tracks .track-label').forEach(el => move(el, 't:' + el.dataset.name));
  },

  // ── Post-sync reveal ──────────────────────────────────────────────────────

  _anim: null,  // { timers: [], items: [], status }

  _animEnabled() {
    return SYNC_BASE_MS > 0 &&
      !(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
  },

  _capturePositions() {
    const map = new Map();
    document.querySelectorAll('#tracks-inner .clip').forEach(el => {
      map.set(el.dataset.key, { left: el.offsetLeft, width: el.offsetWidth, cls: el.className });
    });
    return map;
  },

  _animateSync(before, finalStatus) {
    const items = [...document.querySelectorAll('#tracks-inner .clip')]
      .map(el => ({ el, from: before.get(el.dataset.key), left: el.offsetLeft, width: el.offsetWidth, cls: el.className }))
      .filter(it => it.from);
    if (!items.length) return;
    // Start everything where it was before the sync
    items.forEach(it => {
      it.el.style.transition = 'none';
      it.el.style.left  = it.from.left + 'px';
      it.el.style.width = it.from.width + 'px';
      it.el.className = (it.from.cls || it.cls).replace(/\bselected\b/, '');
    });
    void document.body.offsetHeight;  // commit start positions

    // One smooth glide for every clip together; the confirm sweep follows
    const ms = SYNC_CLIP_MS;
    const anim = { timers: [], items, status: finalStatus };
    this._anim = anim;
    this._drawRuler(this._state.timeline.total_duration);  // no markers until clips land
    // The Unsynced bin and review bar wait until the synced clips have landed
    this._holdPanels(true);
    this.setStatus('Placing clips… · click to skip', 'loading');
    items.forEach(it => {
      it.el.style.transition = `left ${ms}ms ${SYNC_EASE}, width ${ms}ms ${SYNC_EASE}`;
      it.el.style.left  = it.left + 'px';
      it.el.style.width = it.width + 'px';
    });
    // Then a confirm sweep across the timeline, then the panels
    anim.timers.push(setTimeout(() => this._startSweep(anim), ms + 60));
  },

  _holdPanels(on) {
    ['unsynced-panel', 'review-bar', 'unsynced-bar', 'new-bar', 'status-chips', 'footer-stats'].forEach(id =>
      document.getElementById(id).classList.toggle('held', on));
  },

  // Confirm sweep: a line runs left to right across what's on screen; each
  // clip lights up as it passes (green synced, amber to check, red unsynced)
  _startSweep(anim) {
    if (this._anim !== anim) return;
    // Clips have landed: their real look (amber, red) from now on
    anim.items.forEach(it => { it.el.style.transition = ''; it.el.style.animation = ''; it.el.className = it.cls; });
    const scroll = document.getElementById('tracks-scroll');
    const inner = document.getElementById('tracks-inner');
    const x0 = scroll.scrollLeft, x1 = x0 + scroll.clientWidth;
    const line = document.createElement('div');
    line.className = 'sweep-line';
    line.style.height = inner.scrollHeight + 'px';
    inner.appendChild(line);
    anim.sweepLine = line;
    const clips = [...inner.querySelectorAll('.clip')]
      .map(el => ({ el, x: el.offsetLeft, hit: false }))
      .filter(c => c.x < x1)
      .sort((a, b) => a.x - b.x);
    const kind = el => el.classList.contains('unsynced') ? 'bad'
      : el.classList.contains('status-synced_low') ? 'check' : 'ok';
    const t0 = performance.now();
    const ease = t => 1 - Math.pow(1 - t, 2);
    const step = now => {
      if (this._anim !== anim) return;
      const t = Math.min(1, (now - t0) / SWEEP_MS);
      const x = x0 + (x1 - x0) * ease(t);
      line.style.left = x + 'px';
      clips.forEach(c => {
        if (c.hit || x < c.x) return;
        c.hit = true;
        c.el.classList.add('sweep-' + kind(c.el));
        anim.timers.push(setTimeout(() => c.el.classList.remove('sweep-ok', 'sweep-check', 'sweep-bad'), 650));
      });
      if (t < 1) anim.sweepRaf = requestAnimationFrame(step);
      else { line.classList.add('done'); anim.timers.push(setTimeout(() => this._finishAnim(), 250)); }
    };
    anim.sweepRaf = requestAnimationFrame(step);
  },

  _finishAnim() {
    const a = this._anim;
    if (!a) return;
    this._anim = null;
    a.timers.forEach(clearTimeout);
    if (a.sweepRaf) cancelAnimationFrame(a.sweepRaf);
    if (a.sweepLine) a.sweepLine.remove();
    document.querySelectorAll('#tracks-inner .clip.sweep-ok, #tracks-inner .clip.sweep-check, #tracks-inner .clip.sweep-bad')
      .forEach(el => el.classList.remove('sweep-ok', 'sweep-check', 'sweep-bad'));
    a.items.forEach(it => {
      it.el.style.transition = '';
      it.el.style.animation = '';
      it.el.style.left  = it.left + 'px';
      it.el.style.width = it.width + 'px';
      it.el.className = it.cls;
    });
    this._holdPanels(false);
    this._drawRuler(this._state.timeline.total_duration);  // amber markers now
    this.setStatus(a.status.text, a.status.type);

  },

  setStatus(text, type) {
    document.getElementById('status-text').textContent = text;
    document.getElementById('status-text').className   = 'status-text ' + (type || '');
    document.getElementById('status-dot').style.display = type === 'loading' ? 'block' : 'none';
  },

  // ── Render ────────────────────────────────────────────────────────────────

  _render() {
    const s = this._state;
    if (!s) return;

    // Buttons
    document.getElementById('btn-add').disabled    = !s.buttons.add;
    document.getElementById('btn-undo').disabled   = !s.buttons.undo;
    document.getElementById('btn-redo').disabled   = !s.buttons.redo;
    document.getElementById('btn-sync').disabled   = !s.buttons.sync;
    document.getElementById('btn-export').disabled      = !s.buttons.export;
    if (!s.buttons.export) this._closeExport();
    // One bright button: the next step
    const syncNext = s.buttons.sync && (!s.timeline.synced || s.timeline.new > 0);
    document.getElementById('btn-sync').classList.toggle('next', syncNext);
    document.getElementById('btn-export').classList.toggle('next', s.buttons.export && !syncNext);
    document.getElementById('btn-sync').innerHTML = s.timeline.synced ? '&#x21BB; Re-sync' : '&#x21BB; Synchronise';
    document.getElementById('btn-cancel').style.display = s.buttons.cancel ? 'inline-flex' : 'none';
    document.getElementById('btn-clear').style.display  = s.buttons.clear  ? 'block'       : 'none';

    // Status bar
    const stEl = document.getElementById('status-text');
    stEl.textContent = s.status.text;
    stEl.className   = 'status-text' + (s.status.type !== 'default' ? ' ' + s.status.type : '');
    document.getElementById('status-dot').style.display =
      s.status.type === 'loading' ? 'block' : 'none';

    // Drop hint
    document.getElementById('drop-hint').style.display =
      s.tracks.length === 0 ? 'flex' : 'none';

    // Auto-fit only when user hasn't manually zoomed
    if (s.timeline.total_duration > 0 && !this._userZoomed) {
      const vpW = document.getElementById('tracks-scroll').clientWidth - 20;
      this._pps = vpW / s.timeline.total_duration;
    }

    this._renderFooter(s);
    this._renderSidebar(s.tracks);
    this._renderTimeline(s.tracks, s.timeline);
    this._renderUnsyncedPanel(s.tracks, s.timeline);
    this._applyLaneHighlight();
    this._animateBornTrack(s);
    this._drawRuler(s.timeline.total_duration);
    this._updateZoomBar();
  },

  _renderSidebar(tracks) {
    // Labels live in an inner box that's shifted to follow the timeline's
    // vertical scroll (see _initWheelZoom)
    const side = document.getElementById('sidebar-tracks');
    let container = side.querySelector('.sidebar-inner');
    if (!container) {
      container = document.createElement('div');
      container.className = 'sidebar-inner';
      side.appendChild(container);
    }
    container.innerHTML = '';

    tracks.forEach((track, i) => {
      if (i > 0) {
        const div = document.createElement('div');
        div.className = 'sidebar-divider';
        container.appendChild(div);
      }

      const rowH = 86;
      const label = document.createElement('div');
      label.className = 'track-label';
      label.dataset.trackIndex = track.index;
      label.dataset.name = track.display_name;
      label.style.height = rowH + 'px';

      // Drag handle: reorder lanes
      const grip = document.createElement('div');
      grip.className = 'track-grip';
      grip.title = 'Drag to reorder tracks';
      grip.textContent = '⋮⋮';
      grip.addEventListener('mousedown', e => this._startLaneDrag(e, label, track.index));

      const dot  = document.createElement('div');
      dot.className = 'track-dot';
      dot.style.background = track.color;

      const info = document.createElement('div');
      info.className = 'track-info';

      const nameInput = document.createElement('input');
      nameInput.type = 'text';
      nameInput.className = 'track-name-input';
      nameInput.value = track.display_name;
      nameInput.addEventListener('blur',    () => { const a = app._api(); if (a) a.rename_track(track.index, nameInput.value); });
      nameInput.addEventListener('keydown', e => { if (e.key === 'Enter') nameInput.blur(); });

      const meta = document.createElement('div');
      meta.className = 'track-meta';
      const lines = (track.meta && track.meta.length) ? track.meta : (track.user_lane ? ['Empty · drag clips here'] : []);
      lines.forEach(line => {
        const l = document.createElement('div');
        l.textContent = line;
        l.title = line;  // full name on hover when cut short
        meta.appendChild(l);
      });

      info.appendChild(nameInput);
      info.appendChild(meta);

      const removeBtn = document.createElement('button');
      removeBtn.className   = 'track-remove';
      removeBtn.textContent = '✕';
      removeBtn.title       = 'Remove track';
      removeBtn.addEventListener('click', e => { e.stopPropagation(); const a = app._api(); if (a) a.remove_track(track.index); });

      label.appendChild(grip);
      label.appendChild(dot);
      label.appendChild(info);
      label.appendChild(removeBtn);
      container.appendChild(label);
    });

    // + Add track: below every track (and the Unsynced one) so rows stay aligned
    if (tracks.length) {
      const add = document.createElement('button');
      add.type = 'button';
      add.className = 'add-lane';
      add.textContent = '+ Add track';
      add.title = 'Add an empty track, then drag clips onto it';
      add.addEventListener('click', () => { const a = this._api(); if (a) a.add_lane(); });
      container.appendChild(add);
    }

    if (this._followScroll) this._followScroll();
  },

  // Clips that sit on the Unsynced lane (after a sync), with their source track
  _unsyncedItems(tracks, timeline) {
    if (!timeline.synced) return [];
    return tracks.flatMap(t => t.clips.filter(c => c.unsynced_offset != null).map(c => ({ clip: c, track: t })))
      .sort((a, b) => a.clip.unsynced_offset - b.clip.unsynced_offset);
  },

  // Rows are rebuilt on every draw; for a moment the area is empty and the
  // browser would snap the scroll back to the top, so keep it where it was
  _renderTimeline(tracks, timeline) {
    const scroll = document.getElementById('tracks-scroll');
    const top = scroll.scrollTop, left = scroll.scrollLeft;
    this._buildTimeline(tracks, timeline);
    scroll.scrollTop = top;
    scroll.scrollLeft = left;
  },

  _buildTimeline(tracks, timeline) {
    const inner = document.getElementById('tracks-inner');
    inner.classList.toggle('synced-timeline', !!timeline.synced);

    // Remove old track rows, keep the drop hint
    inner.querySelectorAll('.track-row').forEach(el => el.remove());

    if (tracks.length === 0) return;

    const pps   = this._pps;
    const GAP   = 3;
    const MIN_W = 36;
    const vpW   = document.getElementById('tracks-scroll').clientWidth;
    let innerW  = Math.max(timeline.total_duration * pps + 20, vpW);
    inner.style.width = innerW + 'px';

    // Each lane: a row element plus { clip, track, leftPx, widthPx } items
    const lanes = tracks.map(track => {
      const row = document.createElement('div');
      row.className = 'track-row ' + (track.is_video ? 'cam-row' : 'aud-row');
      row.dataset.trackIndex = track.index;
      row.style.setProperty('--lane', track.color);
      let runningPx = 0;
      const items = track.clips.map(clip => {
        let leftPx, widthPx;
        if (timeline.synced) {
          // Unsynced clips go after the synced section, as in the export
          const at = clip.unsynced_offset != null ? clip.unsynced_offset : clip.offset;
          leftPx  = Math.round(at * pps);
          widthPx = Math.max(Math.round(clip.duration * pps) - GAP, 2);
        } else {
          widthPx = Math.max(Math.round(clip.duration * pps) - GAP, MIN_W);
          leftPx  = runningPx;
          runningPx += widthPx + GAP;
        }
        return { clip, track, leftPx, widthPx };
      });
      return { row, items };
    });
    lanes.forEach(({ row, items }) => {
      inner.appendChild(row);
      if (!items.length && row.dataset.trackIndex != null) {
        const t = tracks.find(t => String(t.index) === row.dataset.trackIndex);
        if (t && t.user_lane) {
          const hint = document.createElement('div');
          hint.className = 'lane-hint';
          hint.textContent = 'Drag clips up or down onto this track';
          row.appendChild(hint);
        }
      }
      // Each clip is clipped to where the next one starts, so short clips at
      // low zoom don't spill over their neighbours.
      const byLeft = [...items].sort((a, b) => a.leftPx - b.leftPx);
      for (let k = 0; k < byLeft.length - 1; k++) {
        const room = byLeft[k + 1].leftPx - byLeft[k].leftPx - 1;
        if (room > 0) byLeft[k].widthPx = Math.max(2, Math.min(byLeft[k].widthPx, room));
      }
      items.forEach(it => {
        const right = it.leftPx + it.widthPx + 20;
        if (right > innerW) { innerW = right; inner.style.width = innerW + 'px'; }
        row.appendChild(this._clipEl(it));
      });
    });

    this._applyReviewFocus();
    this._applyFlash();
    // Restore selection highlight if still present
    this._selection = this._selection.filter(k => {
      const [ti, ci] = k.split('|');
      const el = document.querySelector(`#tracks-inner .clip[data-track-index="${ti}"][data-clip-index="${ci}"], #up-clips .clip[data-track-index="${ti}"][data-clip-index="${ci}"]`);
      if (el) el.classList.add('selected');
      return !!el;
    });
    if (this._selected) {
      const { trackIndex: ti, clipIndex: ci } = this._selected;
      const el = document.querySelector(`#tracks-inner .clip[data-track-index="${ti}"][data-clip-index="${ci}"], #up-clips .clip[data-track-index="${ti}"][data-clip-index="${ci}"]`);
      this._selected.tray = false;
      if (!el) this._selected = null;
    }
  },

  // Unsynced clips, docked under the timeline with their waveforms. Drag one
  // up onto the timeline to place it by hand.
  // Unsynced clips now stay on the timeline (red, at the end), so the old
  // bin stays hidden; this just keeps the review bar up to date.
  _renderUnsyncedPanel(tracks, timeline) {
    document.getElementById('unsynced-panel').style.display = 'none';
    this._renderUnsyncedBar(tracks, timeline);
    this._renderReviewBar(tracks, timeline);
    this._alignBars();
  },

  // Buttons sit just after the text, at the same place on every bar
  _alignBars() {
    const texts = [...document.querySelectorAll('.review-bar .rb-text')]
      .filter(t => t.closest('.review-bar').style.display !== 'none');
    texts.forEach(t => { t.style.minWidth = ''; });
    const w = Math.max(0, ...texts.map(t => t.getBoundingClientRect().width));
    texts.forEach(t => { t.style.minWidth = Math.ceil(w) + 'px'; });
  },

  // Red bar: how many clips couldn't be synced, and where they are
  _renderUnsyncedBar(tracks, timeline) {
    const bar = document.getElementById('unsynced-bar');
    const all = timeline.synced && !this._state.buttons.cancel ? this._unsyncedItems(tracks, timeline) : [];
    this._renderNewBar(all.filter(({ clip }) => clip.status === 'new'));
    const items = all.filter(({ clip }) => clip.status !== 'new');
    if (!items.length) { bar.style.display = 'none'; return; }
    const n = items.length;
    bar.style.display = '';
    bar.innerHTML = `<span class="ub-flag">✕ ${n} clip${n === 1 ? '' : 's'} couldn't be synced</span>`;
    this._barButtons(bar, [
      ['View', 'view', 'Zoom to them at the end of the timeline', () => { this._pauseReview(); this._showUnsynced(items); }],
      ['Remove', 'remove', 'Take them off the timeline (Undo brings them back)', () => {
        this._pauseReview();
        const a = this._api();
        if (!a) return;
        // Exactly the clips shown in red, all in one go (one undo step)
        this._removeClips(items.map(({ clip }) => [clip.track_index, clip.clip_index]),
          `Removed ${n} unsynced clip${n === 1 ? '' : 's'} · Undo brings ${n === 1 ? 'it' : 'them'} back`);
      }],
    ]);
  },

  // Grey bar: files added since the last sync. Nothing syncs until the user says so.
  _renderNewBar(items) {
    const bar = document.getElementById('new-bar');
    if (!items.length) { bar.style.display = 'none'; return; }
    const n = items.length;
    const loading = this._state.status.type === 'loading';
    bar.style.display = '';
    bar.innerHTML = `<span class="nb-flag">+ ${n} new clip${n === 1 ? '' : 's'} not synced yet</span>`;
    const [sync] = this._barButtons(bar, [
      ['Sync', 'keep', loading ? 'Wait for the files to finish loading' : 'Sync again with the new files', () => this.runSync()],
      ['View', 'view', 'Zoom to them at the end of the timeline', () => { this._pauseReview(); this._showUnsynced(items); }],
    ]);
    sync.disabled = loading || !this._state.buttons.sync;
  },

  // Buttons on the right of a bar, all the same size: [label, style, tip, onclick]
  _barButtons(bar, list) {
    // The message goes in one block, so the buttons can line up across bars
    const text = document.createElement('span');
    text.className = 'rb-text';
    while (bar.firstChild) text.appendChild(bar.firstChild);
    bar.appendChild(text);
    const box = document.createElement('span');
    box.className = 'rb-actions';
    const btns = list.map(([label, cls, tip, fn]) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'rb-btn ' + cls;
      b.textContent = label;
      b.title = tip;
      b.addEventListener('click', e => { e.stopPropagation(); fn(); });
      box.appendChild(b);
      return b;
    });
    bar.appendChild(box);
    return btns;
  },

  // Zoom to the unsynced section at the end of the timeline
  _showUnsynced(items) {
    const scroll = document.getElementById('tracks-scroll');
    const vpW = scroll.clientWidth;
    const start = Math.min(...items.map(({ clip }) => clip.unsynced_offset));
    const end = Math.max(...items.map(({ clip }) => clip.unsynced_offset + clip.duration));
    const fitPps = this._fitPps();
    const pps = Math.max(fitPps, Math.min(fitPps * 200, (vpW * 0.5) / Math.max(1, end - start)));
    this._animateView(pps, Math.max(0, (start + end) / 2 - vpW / 2 / pps), this._animEnabled() ? FOCUS_MS : 0,
      this._rowScrollTop(items[0].track.index));
  },

  // Review bar: weak matches, one at a time. Keep or Unsync each; the clip
  // being checked is outlined and everything it isn't compared with dims.
  _renderReviewBar(tracks, timeline) {
    const bar = document.getElementById('review-bar');
    const checks = timeline.synced && !this._state.buttons.cancel
      ? tracks.flatMap(t => t.clips.filter(c => c.status === 'synced_low').map(c => ({ clip: c, track: t })))
          .sort((a, b) => a.clip.offset - b.clip.offset)
      : [];
    this._checks = checks;
    if (!checks.length) {
      this._reviewClip = null;
      this._applyReviewFocus();
      bar.style.display = 'none';
      if (this._reviewing) {
        // All checked: back to the whole timeline
        this._reviewing = false;
        this._reviewActive = false;
        this._reviewKey = null;
        this._reviewIdx = 0;
        this._userZoomed = false;
        requestAnimationFrame(() => { this._render(); this.setStatus('All matches checked', 'success'); });
      }
      return;
    }
    if (this._reviewKey) {  // stay on the clip being reviewed if it's still there
      const k = checks.findIndex(({ clip }) => clip.track_index + '|' + clip.clip_index === this._reviewKey);
      if (k >= 0) this._reviewIdx = k;
    }
    this._reviewIdx = Math.min(Math.max(0, this._reviewIdx || 0), checks.length - 1);
    const { clip, track } = checks[this._reviewIdx];
    this._reviewKey = clip.track_index + '|' + clip.clip_index;
    this._reviewing = true;

    const esc = t => String(t).replace(/[&<>"]/g, ch => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[ch]));
    bar.style.display = '';
    if (!this._reviewActive) {
      // Not started: the user looks at the timeline first, then starts checking
      this._reviewClip = null;
      this._applyReviewFocus();
      const n = checks.length;
      bar.innerHTML = `<span class="rb-flag">⚠ ${n} match${n === 1 ? '' : 'es'} to check</span>`;
      this._barButtons(bar, [['Check', 'check', 'Go through them one at a time', () => this._startReview(0)]]);
      return;
    }
    bar.innerHTML = `
      <span class="rb-flag">⚠ Check match</span>
      <span class="rb-dot" style="background:${esc(track.color)}"></span>
      <span class="rb-name" title="${esc(clip.name)}">${esc(clip.name)}</span>
      <span class="rb-where">${esc(track.display_name)} · ${this._fmtTc(clip.offset)}${clip.manual ? ' · moved' : ''}</span>
      <span class="rb-count">${this._reviewIdx + 1} of ${checks.length}</span>`;
    bar.querySelector('.rb-name').addEventListener('click', () => this._goToClip(clip, track));
    this._barButtons(bar, [
      ['Keep', 'keep', 'The sync is right: keep it', () => this._reviewAct('ok')],
      ['Remove', 'remove', 'Wrong match: take it off the timeline (Undo brings it back)', () => this._reviewAct('no')],
    ]);

    this._reviewClip = { clip, track };
    this._applyReviewFocus();
    if (this._reviewGo) {  // follow along on the timeline
      this._reviewGo = false;
      requestAnimationFrame(() => this._goToClip(clip, track));
    }
  },

  _startReview(idx) {
    this._reviewActive = true;
    this._reviewIdx = idx;
    this._reviewKey = null;
    this._reviewGo = true;
    this._render();
  },

  // Leave review where the view is (for looking at something else): nothing dimmed
  _pauseReview() {
    if (!this._reviewActive) return;
    this._reviewActive = false;
    this._reviewClip = null;
    this._applyReviewFocus();
    this._render();
  },

  // Leave review: back to the whole timeline, nothing dimmed
  _stopReview() {
    this._reviewActive = false;
    this._reviewClip = null;
    this._userZoomed = false;
    this._render();
    this.setStatus('', 'default');
  },

  // Focus while reviewing: the clip being checked, plus what it's compared
  // with (clips on other tracks overlapping it); everything else dims.
  // Runs after every timeline redraw.
  _applyReviewFocus() {
    document.querySelectorAll('#tracks-inner .clip.review-current, #tracks-inner .clip.review-dim')
      .forEach(el => el.classList.remove('review-current', 'review-dim'));
    const r = this._reviewClip;
    if (!r || !this._state) return;
    const s0 = r.clip.offset, s1 = r.clip.offset + r.clip.duration;
    this._state.tracks.forEach(t => t.clips.forEach(c => {
      const el = document.querySelector(`#tracks-inner .clip[data-track-index="${c.track_index}"][data-clip-index="${c.clip_index}"]`);
      if (!el) return;
      if (c.track_index === r.clip.track_index && c.clip_index === r.clip.clip_index) { el.classList.add('review-current'); return; }
      const compared = t.index !== r.track.index && c.unsynced_offset == null && c.offset < s1 && c.offset + c.duration > s0;
      if (!compared) el.classList.add('review-dim');
    }));
  },

  // Review: act on the clip being checked. It turns green (Keep) or red
  // (Remove), holds a moment, the view eases out to the whole timeline, then
  // on to the next one.
  _reviewAct(kind) {
    if (this._reviewBusy) return;
    const c = this._checks && this._checks[this._reviewIdx];
    if (!c) return;
    const a = this._api();
    if (!a) return;
    const bar = document.getElementById('review-bar');
    this._reviewBusy = true;
    bar.classList.add('busy');
    this._flash = { key: c.clip.track_index + '|' + c.clip.clip_index, cls: kind === 'ok' ? 'just-kept' : 'just-removed' };
    this._applyFlash();
    this.setStatus(kind === 'ok' ? `Kept ${c.clip.name}` : `Removed ${c.clip.name} · Undo brings it back`, kind === 'ok' ? 'success' : 'default');
    const wait = ms => new Promise(r => setTimeout(r, ms));
    const anim = this._animEnabled();
    (async () => {
      try {
        await wait(anim ? REVIEW_HOLD_MS : 0);
        if (kind !== 'ok') {
          // Remove means remove, as on the red bar: it fades out, a beat, then
          // the whole timeline (Undo brings it back)
          this._reviewClip = null;
          this._applyReviewFocus();
          this._reviewKey = null;
          this._reviewGo = false;
          // (the red stays on while it fades)
          await this._removeClips([[c.clip.track_index, c.clip.clip_index]], `Removed ${c.clip.name} · Undo brings it back`);
          const next = this._reviewActive && this._checks && this._checks[this._reviewIdx];
          if (next) {
            await this._animateView(this._fitPps(), 0, anim ? REVIEW_ZOOM_MS : 0, this._rowScrollTop(next.track.index));
            const el = document.querySelector(`#tracks-inner .clip[data-track-index="${next.clip.track_index}"][data-clip-index="${next.clip.clip_index}"]`);
            if (el) this._selectClip(el);
            this.setStatus(`Checking ${next.clip.name}`, 'default');
          }
          return;
        }
        // Out to the whole timeline, nothing dimmed, so the change is seen in place
        this._reviewClip = null;
        this._applyReviewFocus();
        await this._animateView(this._fitPps(), 0, anim ? REVIEW_ZOOM_MS : 0);
        this._reviewKey = null;  // the clip leaves the list; the same position is now the next one
        this._reviewGo = false;
        await a.confirm_clip(c.clip.track_index, c.clip.clip_index);
        await wait(anim ? REVIEW_PAUSE_MS : 0);
        this._flash = null;
        this._applyFlash();
        // Stay on the whole timeline; the next one to check is highlighted
        // there (its track scrolled into view), and the user zooms in if needed
        const next = this._reviewActive && this._checks && this._checks[this._reviewIdx];
        if (next) {
          await this._animateView(this._fitPps(), 0, anim ? REVIEW_ZOOM_MS : 0, this._rowScrollTop(next.track.index));
          const el = document.querySelector(`#tracks-inner .clip[data-track-index="${next.clip.track_index}"][data-clip-index="${next.clip.clip_index}"]`);
          if (el) this._selectClip(el);
          this.setStatus(`Checking ${next.clip.name}`, 'default');
        }
      } finally {
        this._reviewBusy = false;
        this._flash = null;
        this._applyFlash();
        bar.classList.remove('busy');
      }
    })();
  },

  // Delete clips with a little ceremony: they fade out, a beat so it's seen,
  // then out to the whole timeline. One undo step.
  async _removeClips(pairs, note) {
    const a = this._api();
    if (!a || !pairs.length) return;
    const anim = this._animEnabled();
    const wait = ms => new Promise(r => setTimeout(r, anim ? ms : 0));
    this._removing = new Set(pairs.map(p => p.join('|')));
    this._applyFlash();
    // Fade, a beat with the gap showing, then out to the whole timeline, all
    // while the clips are still there (hidden) so nothing jumps
    await wait(REMOVE_FADE_MS);
    await wait(REMOVE_PAUSE_MS);
    await this._animateView(this._fitPps(), 0, anim ? REVIEW_ZOOM_MS : 0);
    // Now delete. The timeline gets shorter: hold the zoom through the redraw,
    // then ease to the new whole-timeline fit
    this._selection = []; this._selected = null;
    this._note = note;
    this._userZoomed = true;
    await a.delete_clips(pairs);
    this._removing = null;
    await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
    await this._animateView(this._fitPps(), 0, anim ? 250 : 0);
  },

  // Green/red mark on a clip just kept or removed, and the fade on clips
  // being deleted; survives redraws
  _applyFlash() {
    document.querySelectorAll('#tracks-inner .clip.just-kept, #tracks-inner .clip.just-removed')
      .forEach(el => el.classList.remove('just-kept', 'just-removed'));
    if (this._removing) this._removing.forEach(k => {
      const [ti, ci] = k.split('|');
      const el = document.querySelector(`#tracks-inner .clip[data-track-index="${ti}"][data-clip-index="${ci}"]`);
      if (el) el.classList.add('removing');
    });
    const f = this._flash;
    if (!f) return;
    const [ti, ci] = f.key.split('|');
    const el = document.querySelector(`#tracks-inner .clip[data-track-index="${ti}"][data-clip-index="${ci}"]`);
    if (el) el.classList.add(f.cls);
  },

  // A track just made by dropping clips below the last one: it opens up
  // from nothing and glows briefly, its name sliding in alongside
  _animateBornTrack(s) {
    const i = this._bornTrack;
    if (i == null || s.tracks.length <= i) return;
    this._bornTrack = null;
    if (!this._animEnabled()) return;
    const els = document.querySelectorAll(`#tracks-inner .track-row[data-track-index="${i}"], #sidebar-tracks .track-label[data-track-index="${i}"]`);
    els.forEach(el => {
      el.classList.remove('track-born');
      void el.offsetWidth;
      el.classList.add('track-born');
      setTimeout(() => el.classList.remove('track-born'), 1600);
    });
  },

  // R: back to the whole timeline, top track first
  _resetView() {
    if (!this._state || !this._state.timeline.total_duration) return;
    this._animateView(this._fitPps(), 0, this._animEnabled() ? FOCUS_MS : 0, 0);
  },

  // Ease out to the whole timeline (if zoomed in)
  _showWhole() {
    const scroll = document.getElementById('tracks-scroll');
    if (!this._userZoomed && scroll.scrollLeft < 1) return;
    this._animateView(this._fitPps(), 0, this._animEnabled() ? FOCUS_MS : 0);
  },

  // Vertical scroll that brings a track fully into view (null: already is)
  _rowScrollTop(ti) {
    const scroll = document.getElementById('tracks-scroll');
    const row = document.querySelector(`#tracks-inner .track-row[data-track-index="${ti}"]`);
    if (!row) return null;
    const top = row.offsetTop, bottom = top + row.offsetHeight;
    const pad = 24;
    const fits = top - pad >= scroll.scrollTop && bottom + pad <= scroll.scrollTop + scroll.clientHeight;
    if (fits) return null;
    // Out of view or at an edge: bring it to the middle
    const max = scroll.scrollHeight - scroll.clientHeight;
    return Math.max(0, Math.min(max, top + row.offsetHeight / 2 - scroll.clientHeight / 2));
  },

  _fitPps() {
    const vpW = document.getElementById('tracks-scroll').clientWidth - 20;
    return vpW / Math.max(1, this._state.timeline.total_duration);
  },

  // Zoom and left edge (seconds) that show a clip well, as _focusOnTimeline does
  _focusView(c) {
    const vpW = document.getElementById('tracks-scroll').clientWidth;
    const fit = this._fitPps();
    // Just enough zoom to see the clip clearly, with plenty around it
    let pps = this._pps;
    const want = vpW * FOCUS_CLIP_SHARE;
    if (c.duration * pps < want * 0.6) pps = Math.max(fit, Math.min(fit * 200, want / Math.max(c.duration, 0.5)));
    return { pps, left: Math.max(0, c.offset + c.duration / 2 - vpW / 2 / pps) };
  },

  // Ease zoom and horizontal scroll to a new view. Resolves when done.
  _animateView(toPps, toLeft, ms, toTop = null) {
    const scroll = document.getElementById('tracks-scroll');
    const fit = this._fitPps();
    const p0 = this._pps, l0 = scroll.scrollLeft / this._pps;
    const y0 = scroll.scrollTop, y1 = toTop == null ? y0 : toTop;
    let k = 1;  // progress, for the vertical scroll
    const set = (pps, left) => {
      this._pps = pps;
      this._userZoomed = pps > fit * 1.01;
      this._viewAnimating = true;
      this._rerenderForZoom();
      this._viewAnimating = false;
      scroll.scrollLeft = Math.max(0, left * pps);
      scroll.scrollTop = y0 + (y1 - y0) * k;
      this._drawVisibleWaves();  // sharp in every frame, same as at rest
      this._drawRuler(this._state.timeline.total_duration);
      this._updateZoomBar();
    };
    const token = this._viewAnim = (this._viewAnim || 0) + 1;  // a newer move takes over
    if (!ms || (Math.abs(toPps - p0) < 1e-6 && Math.abs(toLeft - l0) < 1e-3 && Math.abs(y1 - y0) < 1)) { set(toPps, toLeft); return Promise.resolve(); }
    return new Promise(resolve => {
      const t0 = performance.now();
      const ease = t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
      const step = now => {
        if (token !== this._viewAnim) { resolve(); return; }
        const t = Math.min(1, (now - t0) / ms);
        k = ease(t);
        set(p0 * Math.pow(toPps / p0, k), l0 + (toLeft - l0) * k);  // zoom eases in log space
        if (t < 1) requestAnimationFrame(step); else resolve();
      };
      requestAnimationFrame(step);
    });
  },

  _reviewStep(dir) {
    const n = this._checks ? this._checks.length : 0;
    if (!n) return;
    this._reviewIdx = (this._reviewIdx + dir + n) % n;
    this._reviewKey = null;
    this._reviewGo = true;
    this._render();
  },

  _goToClip(clip, track) {
    this._focusOnTimeline(clip, track);
    const el = document.querySelector(`#tracks-inner .clip[data-track-index="${clip.track_index}"][data-clip-index="${clip.clip_index}"]`);
    if (el) this._selectClip(el);
    this.setStatus(`Checking ${clip.name}`, 'default');
  },

  _clipEl({ clip, track, leftPx, widthPx }) {
    const el = document.createElement('div');
    el.className = 'clip status-' + clip.status +
      (widthPx < 44 ? ' narrow' : widthPx < 96 ? ' medium' : '');
    el.style.left  = leftPx + 'px';
    el.style.width = widthPx + 'px';
    el.dataset.trackIndex = clip.track_index;
    el.dataset.key = clip.track_index + '|' + clip.name;
    el.dataset.clipIndex  = clip.clip_index;
    el.title = this._tooltip(clip);

    // Colour always says which device; status is shown by outline/badge
    if (clip.status !== 'loading') {
      el.style.background  = this._rgba(track.color, 0.18);
      el.style.borderColor = this._rgba(track.color, 0.40);
      el.style.color       = '#fff';
    }
    if (this._filter && !this._matchesFilter(clip)) el.classList.add('dimmed');

    const wc = document.createElement('canvas');
    wc.className = 'clip-wave';
    el.appendChild(wc);

    const lbl = document.createElement('div');
    lbl.className = 'clip-label';
    const nameEl = document.createElement('div');
    nameEl.className = 'clip-name';
    nameEl.textContent = clip.name;
    const statusEl = document.createElement('div');
    statusEl.className = 'clip-status';
    statusEl.textContent = this._statusLabel(clip);
    lbl.appendChild(nameEl);
    lbl.appendChild(statusEl);
    el.appendChild(lbl);
    if (clip.tc_used) {  // shown once timecode sync places or confirms a clip
      const tc = document.createElement('div');
      tc.className = 'clip-tc';
      tc.textContent = 'TC';
      el.appendChild(tc);
    }

    if (clip.manual) el.classList.add('manual');
    // Added since the last sync: waiting, not failed (grey until Re-sync)
    if (clip.status === 'new') el.classList.add('pending-new');
    else if (clip.unsynced_offset != null) el.classList.add('unsynced');
    el.addEventListener('mousedown', e => {
      if (e.button !== 0) return;
      // A focused text box (a track name) would swallow , and . otherwise
      if (document.activeElement && document.activeElement.tagName === 'INPUT') document.activeElement.blur();
      const toggle = e.shiftKey || e.ctrlKey || e.metaKey;
      const already = el.classList.contains('selected');
      if (toggle) this._selectClip(el, 'toggle');
      else if (!already) this._selectClip(el);
      this._startDrag(e, el, clip, !toggle && already);
    });
    el.addEventListener('contextmenu', e => {
      e.preventDefault();
      if (!el.classList.contains('selected')) this._selectClip(el);
      this._showMenu(e.clientX, e.clientY, clip);
    });
    if (clip.waveform && clip.waveform.length > 0) {
      this._wavesSeen = this._wavesSeen || new Set();
      if (!this._wavesSeen.has(clip.name)) { this._wavesSeen.add(clip.name); wc.classList.add('wave-in'); }
      // Weak matches read as amber all over, waveform included
      const waveColor = clip.status === 'synced_low' ? '#ffe08a' : track.color;
      // A cached picture shows at once (zooms and redraws never flash blank);
      // the sharp version is drawn over it a frame later
      wc.style.backgroundImage = `url(${this._wavePicture(clip, waveColor)})`;
      wc._wave = { peaks: clip.waveform, color: waveColor };
      if (!this._viewAnimating) requestAnimationFrame(() => this._drawWave(wc, clip.waveform, waveColor));
    }
    return el;
  },

  // One small picture per clip and colour, stretched to fit while the sharp
  // canvas catches up
  // Waveform as bars: one rounded bar per few pixels, each the loudest
  // point it covers. Silence still shows as a thin line of dots.
  _waveBars(ctx, peaks, W, H, color) {
    const BAR = 2, GAP = 1, step = BAR + GAP;
    const n = Math.max(1, Math.floor(W / step));
    const mid = H / 2;
    ctx.fillStyle = color;
    ctx.beginPath();
    for (let b = 0; b < n; b++) {
      const i0 = Math.floor(b * peaks.length / n), i1 = Math.max(i0 + 1, Math.floor((b + 1) * peaks.length / n));
      let p = 0;
      for (let i = i0; i < i1 && i < peaks.length; i++) if (peaks[i] > p) p = peaks[i];
      const h = Math.max(1.5, p * H * WAVE_HEIGHT);
      const x = b * step, y = mid - h / 2;
      if (ctx.roundRect) ctx.roundRect(x, y, BAR, h, 1); else ctx.rect(x, y, BAR, h);
    }
    ctx.fill();
  },

  _wavePicture(clip, color) {
    this._wavePics = this._wavePics || new Map();
    const key = clip.track_index + '|' + clip.name + '|' + color + '|' + clip.waveform.length;
    let url = this._wavePics.get(key);
    if (url) return url;
    const peaks = clip.waveform;
    const W = Math.max(64, Math.min(peaks.length, 2400)), H = 64;
    const c = document.createElement('canvas');
    c.width = W; c.height = H;
    const ctx = c.getContext('2d');
    this._waveBars(ctx, peaks, W, H, color);
    url = c.toDataURL();
    this._wavePics.set(key, url);
    return url;
  },

  // Draw the waveforms of clips on screen now (during an eased zoom, so each
  // frame looks the same as the final one)
  _drawVisibleWaves() {
    const scroll = document.getElementById('tracks-scroll');
    const x0 = scroll.scrollLeft, x1 = x0 + scroll.clientWidth;
    const y0 = scroll.scrollTop, y1 = y0 + scroll.clientHeight;
    document.querySelectorAll('#tracks-inner canvas.clip-wave').forEach(wc => {
      const el = wc.parentElement, row = el && el.parentElement;
      if (!wc._wave || !row) return;
      const l = el.offsetLeft, t = row.offsetTop;
      if (l > x1 || l + el.offsetWidth < x0 || t > y1 || t + row.offsetHeight < y0) return;
      this._drawWave(wc, wc._wave.peaks, wc._wave.color);
    });
  },

  _drawWave(canvas, peaks, color) {
    const parent = canvas.parentElement;
    const W = parent.clientWidth;
    const H = parent.clientHeight;
    if (!W || !H) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width  = W * dpr;
    canvas.height = H * dpr;
    canvas.style.width  = '100%';  // follows the clip if it resizes (sync reveal)
    canvas.style.height = '100%';
    const ctx = canvas.getContext('2d');
    ctx.scale(dpr, dpr);
    this._waveBars(ctx, peaks, W, H, color);
    canvas.style.backgroundImage = '';  // the sharp one is in
  },

  _drawRuler(totalSecs) {
    const canvas = document.getElementById('ruler');
    if (!canvas) return;
    if (!totalSecs) {
      canvas.getContext('2d').clearRect(0, 0, canvas.width, canvas.height);
      return;
    }
    const W   = canvas.parentElement.clientWidth;
    const H   = 36;
    const dpr = window.devicePixelRatio || 1;
    canvas.width  = W * dpr;
    canvas.height = H * dpr;
    canvas.style.width  = W + 'px';
    canvas.style.height = H + 'px';
    const ctx = canvas.getContext('2d');
    ctx.scale(dpr, dpr);

    const pps = this._pps;
    // The ruler sits outside the scrolling area: draw the visible stretch
    const sx  = document.getElementById('tracks-scroll').scrollLeft;
    const ivs = [
      { major: 5,    sub: 5, fmt: s => s + 's'                    },
      { major: 10,   sub: 5, fmt: s => s + 's'                    },
      { major: 30,   sub: 6, fmt: s => s + 's'                    },
      { major: 60,   sub: 4, fmt: s => Math.round(s/60) + 'm'     },
      { major: 120,  sub: 4, fmt: s => Math.round(s/60) + 'm'     },
      { major: 300,  sub: 5, fmt: s => Math.round(s/60) + 'm'     },
      { major: 600,  sub: 5, fmt: s => Math.round(s/60) + 'm'     },
      { major: 1800, sub: 6, fmt: s => Math.round(s/60) + 'm'     },
      { major: 3600, sub: 4, fmt: s => Math.round(s/3600) + 'h'   },
    ];
    let iv = ivs[ivs.length - 1];
    for (const c of ivs) { if (c.major * pps >= 80) { iv = c; break; } }

    const subIv  = iv.major / iv.sub;
    const nTicks = Math.ceil(totalSecs / subIv) + 1;
    for (let i = Math.max(0, Math.floor(sx / pps / subIv)); i < nTicks; i++) {
      const t = i * subIv;
      const x = t * pps - sx;
      if (x > W) break;
      const maj = Math.abs(t % iv.major) < 0.01;
      ctx.strokeStyle = maj ? 'rgba(255,255,255,0.4)' : 'rgba(255,255,255,0.12)';
      ctx.lineWidth   = maj ? 0.75 : 0.5;
      ctx.beginPath();
      ctx.moveTo(x, H);
      ctx.lineTo(x, H - (maj ? 14 : 6));
      ctx.stroke();
    }

    ctx.fillStyle    = 'rgba(255,255,255,0.55)';
    ctx.font         = '11px "Cascadia Mono", Consolas, monospace';
    ctx.textBaseline = 'top';
    const nLabels    = Math.ceil(totalSecs / iv.major);
    let lastX        = -999;
    for (let i = Math.max(0, Math.floor(sx / pps / iv.major) - 1); i <= nLabels; i++) {
      const t  = i * iv.major;
      const x  = t * pps - sx;
      if (x < -60) continue;
      if (x > W + 2) break;
      const lbl = iv.fmt(t);
      const tw  = ctx.measureText(lbl).width;
      const lx  = Math.max(2, Math.min(x - tw / 2, W - tw - 2));
      if (lx > lastX + tw + 14) { ctx.fillText(lbl, lx, 6); lastX = lx; }
    }

    // Clips to check: amber markers, visible at any zoom
    ctx.fillStyle = '#f7c94f';
    if (!this._anim) (this._state ? this._state.tracks : []).flatMap(t => t.clips).filter(c => c.status === 'synced_low').forEach(c => {
      const cx = (c.offset + c.duration / 2) * pps - sx;  // middle of the clip
      if (cx < -6 || cx > W + 6) return;
      ctx.beginPath();
      ctx.moveTo(cx - 5, H - 9);
      ctx.lineTo(cx + 5, H - 9);
      ctx.lineTo(cx, H - 1);
      ctx.closePath();
      ctx.fill();
    });
  },

  // ── Zoom & scroll bar ─────────────────────────────────────────────────────

  _zoom(factor, anchorX) {
    const totalDur = this._state && this._state.timeline.total_duration;
    if (!totalDur) return;
    const scroll = document.getElementById('tracks-scroll');
    const vpW    = scroll.clientWidth;
    const fitPps = (vpW - 20) / totalDur;

    const timeAtAnchor = (scroll.scrollLeft + anchorX) / this._pps;
    const newPps = Math.min(Math.max(this._pps * factor, fitPps), fitPps * 200);
    if (Math.abs(newPps - this._pps) < 0.0001) return;

    this._pps        = newPps;
    this._userZoomed = newPps > fitPps * 1.01;
    this._rerenderForZoom();
    scroll.scrollLeft = Math.max(0, timeAtAnchor * this._pps - anchorX);
  },

  _rerenderForZoom() {
    if (!this._state) return;
    this._renderTimeline(this._state.tracks, this._state.timeline);
    this._drawRuler(this._state.timeline.total_duration);
    this._updateZoomBar();
  },

  _updateZoomBar() {
    const totalDur = this._state && this._state.timeline.total_duration;
    const bar    = document.getElementById('zoombar');
    const handle = document.getElementById('zoombar-handle');
    if (!bar || !handle) return;
    if (!totalDur) { handle.style.width = '100%'; handle.style.left = '0'; return; }

    const scroll = document.getElementById('tracks-scroll');
    const bw  = bar.clientWidth;
    const vpW = scroll.clientWidth;
    const tp  = totalDur * this._pps;

    const left  = Math.max(0, (scroll.scrollLeft / tp) * bw);
    const width = Math.max(16, Math.min((vpW / tp) * bw, bw - left));
    handle.style.left  = left + 'px';
    handle.style.width = width + 'px';
  },

  _initZoomBar() {
    const bar    = document.getElementById('zoombar');
    const handle = document.getElementById('zoombar-handle');
    const gripL  = document.getElementById('zoombar-grip-l');
    const gripR  = document.getElementById('zoombar-grip-r');
    const scroll = document.getElementById('tracks-scroll');

    scroll.addEventListener('scroll', () => {
      this._updateZoomBar();
      if (this._state) this._drawRuler(this._state.timeline.total_duration);
    });

    let drag = null;

    const startDrag = (e, type) => {
      e.preventDefault();
      e.stopPropagation();
      const totalDur = this._state && this._state.timeline.total_duration;
      if (!totalDur) return;
      const bw = bar.clientWidth;
      const tp = totalDur * this._pps;
      drag = {
        type,
        x0:  e.clientX,
        sl0: scroll.scrollLeft,
        pps0: this._pps,
        hl0: (scroll.scrollLeft / tp) * bw,
        hr0: ((scroll.scrollLeft + scroll.clientWidth) / tp) * bw,
      };
      document.addEventListener('mousemove', onMove);
      document.addEventListener('mouseup',   onUp);
    };

    const onMove = (e) => {
      if (!drag) return;
      const totalDur = this._state.timeline.total_duration;
      const bw  = bar.clientWidth;
      const vpW = scroll.clientWidth;
      const dx  = e.clientX - drag.x0;

      if (drag.type === 'pan') {
        const tp = totalDur * drag.pps0;
        scroll.scrollLeft = Math.max(0, drag.sl0 + (dx / bw) * tp);
        this._updateZoomBar();
      } else {
        const fitPps = (vpW - 20) / totalDur;
        let hl = drag.hl0, hr = drag.hr0;
        if (drag.type === 'left')  hl = Math.max(0,  Math.min(drag.hl0 + dx, drag.hr0 - 12));
        if (drag.type === 'right') hr = Math.max(drag.hl0 + 12, Math.min(drag.hr0 + dx, bw));
        const leftT  = (hl / bw) * totalDur;
        const rightT = (hr / bw) * totalDur;
        const newPps = Math.min(Math.max(vpW / (rightT - leftT), fitPps), fitPps * 200);
        this._pps        = newPps;
        this._userZoomed = newPps > fitPps * 1.01;
        this._rerenderForZoom();
        scroll.scrollLeft = leftT * this._pps;
      }
    };

    const onUp = () => {
      drag = null;
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup',   onUp);
    };

    handle.addEventListener('mousedown', e => {
      if (e.target === gripL || e.target === gripR) return;
      startDrag(e, 'pan');
    });
    gripL.addEventListener('mousedown', e => startDrag(e, 'left'));
    gripR.addEventListener('mousedown', e => startDrag(e, 'right'));
  },

  _initWheelZoom() {
    const scroll = document.getElementById('tracks-scroll');
    scroll.addEventListener('wheel', (e) => {
      e.preventDefault();
      const px = e.deltaMode === 1 ? 33 : 1;  // some wheels report lines, not pixels
      const dy = e.deltaY * px, dx = e.deltaX * px;
      if (e.shiftKey) {
        // Shift+scroll: up and down through the tracks (as in Resolve). The
        // browser reports Shift+wheel as sideways movement, so take either.
        scroll.scrollTop += (dy || dx);
      } else if (e.altKey || Math.abs(dx) > Math.abs(dy)) {
        // Alt+scroll, or a sideways trackpad swipe: move along the timeline
        scroll.scrollLeft += (Math.abs(dy) > Math.abs(dx) ? dy : dx) * 1.5;
        this._updateZoomBar();
      } else {
        // Scroll (and trackpad pinch, which arrives as Ctrl+scroll): zoom at the mouse
        const factor = Math.exp(-Math.max(-300, Math.min(300, dy)) * 0.0015);
        this._zoom(factor, e.clientX - scroll.getBoundingClientRect().left);
      }
    }, { passive: false });
    // The sidebar's track list follows the timeline up and down
    const side = document.getElementById('sidebar-tracks');
    const follow = () => {
      const inner = side.querySelector('.sidebar-inner');
      if (inner) inner.style.transform = `translateY(${-scroll.scrollTop}px)`;
    };
    scroll.addEventListener('scroll', follow);
    // The sidebar's track list ends exactly where the tracks area does, so
    // labels never sit beside the review bar or the Unsynced bin
    const fit = () => {
      const top = scroll.getBoundingClientRect().top - side.getBoundingClientRect().top;
      side.style.height = Math.max(0, scroll.clientHeight + top) + 'px';
    };
    new ResizeObserver(fit).observe(scroll);
    window.addEventListener('resize', fit);
    fit();
    this._followScroll = follow;
    side.addEventListener('wheel', e => {
      e.preventDefault();
      scroll.scrollTop += (e.deltaY || e.deltaX);
    }, { passive: false });
  },

  // ── Clip selection ────────────────────────────────────────────────────────

  // mode: 'only' (just this clip) or 'toggle' (Shift/Ctrl+click)
  _selectClip(el, mode = 'only') {
    const key = el.dataset.trackIndex + '|' + el.dataset.clipIndex;
    if (mode === 'only') {
      document.querySelectorAll('.clip.selected').forEach(c => c.classList.remove('selected'));
      this._selection = [];
    }
    if (mode === 'toggle' && el.classList.contains('selected')) {
      el.classList.remove('selected');
      this._selection = this._selection.filter(k => k !== key);
      if (this._selected && this._selected.trackIndex + '|' + this._selected.clipIndex === key) this._selected = null;
    } else {
      el.classList.add('selected');
      if (!this._selection.includes(key)) this._selection.push(key);
      this._selected = {
        trackIndex: parseInt(el.dataset.trackIndex),
        clipIndex:  parseInt(el.dataset.clipIndex),
        tray:       false,
      };
    }
    this._applyLaneHighlight();
  },

  _clipFor(el) {
    const ti = +el.dataset.trackIndex, ci = +el.dataset.clipIndex;
    const t = this._state && this._state.tracks.find(t => t.index === ti);
    return t && t.clips.find(c => c.clip_index === ci);
  },

  // Box select: drag on empty timeline to select every clip the box touches
  // (Shift/Ctrl adds to the selection)
  _startMarquee(e) {
    const inner = document.getElementById('tracks-inner');
    const r = inner.getBoundingClientRect();
    const add = e.shiftKey || e.ctrlKey || e.metaKey;
    this._marquee = { x0: e.clientX - r.left, y0: e.clientY - r.top, add, base: add ? [...this._selection] : [], box: null };
    e.preventDefault();
  },

  _onMarqueeMove(e) {
    const m = this._marquee;
    if (!m) return;
    const inner = document.getElementById('tracks-inner');
    const r = inner.getBoundingClientRect();
    const x = e.clientX - r.left, y = e.clientY - r.top;
    if (!m.box) {
      if (Math.hypot(x - m.x0, y - m.y0) < 4) return;
      m.box = document.createElement('div');
      m.box.className = 'marquee';
      inner.appendChild(m.box);
    }
    const L = Math.min(x, m.x0), T = Math.min(y, m.y0), W = Math.abs(x - m.x0), H = Math.abs(y - m.y0);
    Object.assign(m.box.style, { left: L + 'px', top: T + 'px', width: W + 'px', height: H + 'px' });
    const keys = new Set(m.base);
    inner.querySelectorAll('.clip').forEach(el => {
      const c = el.getBoundingClientRect();
      const cl = c.left - r.left, ct = c.top - r.top;
      const hit = cl < L + W && cl + c.width > L && ct < T + H && ct + c.height > T;
      const key = el.dataset.trackIndex + '|' + el.dataset.clipIndex;
      if (hit) keys.add(key);
      el.classList.toggle('selected', keys.has(key));
    });
    this._selection = [...keys];
    const last = this._selection[this._selection.length - 1];
    this._selected = last ? { trackIndex: +last.split('|')[0], clipIndex: +last.split('|')[1], tray: false } : null;
    const n = keys.size;
    this.setStatus(n ? `${n} clip${n === 1 ? '' : 's'} selected · drag to move · Delete to remove` : '', 'default');
  },

  _onMarqueeEnd() {
    const m = this._marquee;
    this._marquee = null;
    if (m && m.box) { m.box.remove(); this._applyLaneHighlight(); }
  },

  _selectedPairs() {
    return this._selection.map(k => k.split('|').map(Number));
  },

  _showMenu(x, y, clip) {
    const menu = document.getElementById('ctx-menu');
    const anyFailed = this._state && this._state.tracks.some(t => t.clips.some(c => c.status === 'failed'));
    menu.querySelector('[data-action="remove-unsynced"]').disabled = !anyFailed;
    // Native hover tooltips would sit on top of the menu; park them meanwhile
    document.querySelectorAll('.clip[title]').forEach(e => {
      e.dataset.tip = e.title; e.removeAttribute('title');
    });
    menu.style.display = 'flex';
    // keep on screen
    const w = menu.offsetWidth, h = menu.offsetHeight;
    menu.style.left = Math.min(x, window.innerWidth  - w - 4) + 'px';
    menu.style.top  = Math.min(y, window.innerHeight - h - 4) + 'px';
  },

  _hideMenu() {
    const menu = document.getElementById('ctx-menu');
    if (menu.style.display === 'none') return;
    menu.style.display = 'none';
    document.querySelectorAll('[data-tip]').forEach(e => { e.title = e.dataset.tip; delete e.dataset.tip; });
  },

  // ── Footer: counters as filters, footage stats ───────────────────────────

  _matchesFilter(clip) {
    if (this._filter === 'unsynced') return clip.status === 'failed' || clip.status === 'new';
    return { synced: 'synced', review: 'synced_low' }[this._filter] === clip.status;
  },

  _setFilter(f) {
    this._filter = this._filter === f ? null : f;
    this._render();
  },

  // "N to check" / "N unsynced": highlight that kind and go to the next one
  _goToNext(key) {
    const s = this._state;
    if (!s) return;
    const want = key === 'review' ? (c => c.status === 'synced_low') : (c => c.unsynced_offset != null);
    const at = c => c.unsynced_offset != null ? c.unsynced_offset : c.offset;
    const list = s.tracks.flatMap(t => t.clips.filter(want).map(c => ({ c, t })))
      .sort((a, b) => (at(a.c) - at(b.c)) || (a.t.index - b.t.index));
    if (!list.length) return;
    const k = this._goKey === key ? (this._goIdx + 1) % list.length : 0;
    this._goKey = key; this._goIdx = k;
    this._filter = key;
    this._render();
    const { c, t } = list[k];
    const sel = `.clip[data-track-index="${c.track_index}"][data-clip-index="${c.clip_index}"]`;
    if (key === 'review') {
      const ri = (this._checks || []).findIndex(x => x.clip.track_index === c.track_index && x.clip.clip_index === c.clip_index);
      if (ri >= 0) { this._startReview(ri); return; }
      this._focusOnTimeline(c, t);
      const el = document.querySelector('#tracks-inner ' + sel);
      if (el) this._selectClip(el);
      this.setStatus(`To check ${k + 1} of ${list.length}: ${c.name} on ${t.display_name} · drag or nudge (, .) it if it's out`, 'default');
    } else {
      this._pauseReview();
      this._focusOnTimeline({ ...c, offset: c.unsynced_offset }, t);
      const el = document.querySelector('#tracks-inner ' + sel);
      if (el) this._selectClip(el);
      this.setStatus(`Unsynced ${k + 1} of ${list.length}: ${c.name}${c.note ? ' · ' + c.note : ''} · drag it sideways into place`, 'default');
    }
  },

  // Ease the view to a clip: zoom just enough to see it, centre it, and
  // scroll to its track
  _focusOnTimeline(c, t, ms = this._animEnabled() ? FOCUS_MS : 0) {
    const scroll = document.getElementById('tracks-scroll');
    const { pps, left } = this._focusView(c);
    return this._animateView(pps, left, ms, this._rowScrollTop(t.index));
  },

  _renderFooter(s) {
    const done = s.timeline.synced && !s.buttons.cancel;
    const chips = document.getElementById('status-chips');
    const stats = document.getElementById('footer-stats');
    chips.style.display = done ? 'flex' : 'none';
    stats.style.display = done ? '' : 'none';
    if (!done) { this._filter = null; return; }

    const all = s.tracks.flatMap(t => t.clips);
    const n = st => all.filter(c => c.status === st).length;
    const counts = [
      ['synced',   'ok',       n('synced'),     'synced'],
      ['review',   'review',   n('synced_low'), 'to check'],
      ['unsynced', 'unplaced', n('failed'),     'unsynced'],
    ];
    chips.innerHTML = '';
    counts.forEach(([key, cls, count, label]) => {
      if (count === 0 && key !== 'synced') return;
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'chip ' + cls + (this._filter === key ? ' active' : '');
      b.innerHTML = '<i></i>';
      b.appendChild(document.createTextNode(count + ' ' + label));
      if (key === 'synced') {
        b.title = this._filter === key ? 'Show all clips' : 'Highlight these clips';
        b.addEventListener('click', () => {
          this._pauseReview();
          this._setFilter(key);
          if (this._filter === key) this._showWhole();  // highlight them across the whole timeline
        });
      } else {
        b.title = count > 1 ? 'Go to the next one (click again for the one after)' : 'Go to it';
        b.addEventListener('click', () => this._goToNext(key));
      }
      chips.appendChild(b);
    });

    const nClips = all.length;
    // Timeline length as HH:MM:SS (what the export will run to)
    const T = Math.round(s.timeline.total_duration || 0);
    const hms = [Math.floor(T / 3600), Math.floor(T / 60) % 60, T % 60].map(v => String(v).padStart(2, '0')).join(':');
    stats.textContent = `${nClips} clips · ${this._fmtLong(s.timeline.footage)} footage · timeline ${hms}`;
  },

  _fmtLong(secs) {
    if (secs < 60) return Math.round(secs) + 's';
    const h = Math.floor(secs / 3600), m = Math.round((secs % 3600) / 60);
    return h > 0 ? `${h}h ${String(m).padStart(2, '0')}m` : `${Math.max(m, secs > 0 ? 1 : 0)}m`;
  },

  // An unsynced clip sits on the Unsynced lane, so show which lane it came from.
  _applyLaneHighlight() {
    document.querySelectorAll('.lane-hl').forEach(e => e.classList.remove('lane-hl'));
    const sel = this._selected;
    if (!sel || !sel.tray) return;
    document.querySelectorAll(`.track-row[data-track-index="${sel.trackIndex}"], .track-label[data-track-index="${sel.trackIndex}"]`)
      .forEach(e => e.classList.add('lane-hl'));
  },

  _deleteSelected() {
    const pairs = this._selectedPairs();
    if (!pairs.length) return;
    const n = pairs.length;
    this._removeClips(pairs, `Removed ${n} clip${n === 1 ? '' : 's'} · Undo brings ${n === 1 ? 'it' : 'them'} back`);
  },

  // ── Actions ───────────────────────────────────────────────────────────────

  _api() {
    const T = window.__TAURI__;
    if (!T || !T.core) {
      this.setStatus('App bridge not ready — please wait a moment and try again', 'error');
      return null;
    }
    const inv = T.core.invoke;
    return {
      add_files:            ()           => inv('add_files'),
      add_files_from_paths: (paths)      => inv('add_files_from_paths', { paths }),
      run_sync:             ()           => inv('run_sync'),
      cancel_sync:          ()           => inv('cancel_sync'),
      remove_track:         (index)      => inv('remove_track', { index }),
      rename_track:         (index, name)=> inv('rename_track', { index, name }),
      delete_clip:          (ti, ci)     => inv('delete_clip', { trackIndex: ti, clipIndex: ci }),
      delete_clips:         (clips)      => inv('delete_clips', { clips }),
      add_lane:             ()           => inv('add_lane'),
      confirm_clip:         (ti, ci)     => inv('confirm_clip', { trackIndex: ti, clipIndex: ci }),
      unsync_clip:          (ti, ci)     => inv('unsync_clip', { trackIndex: ti, clipIndex: ci }),
      confirm_all:          ()           => inv('confirm_all'),
      move_to_lane:         (clips, target) => inv('move_to_lane', { clips, target }),
      move_track:           (from, to)   => inv('move_track', { from, to }),
      move_clip:            (ti, ci, offset, nudge) => inv('move_clip', { trackIndex: ti, clipIndex: ci, offset, nudge }),
      undo:                 ()           => { app._selection = []; app._selected = null; return inv('undo'); },
      redo:                 ()           => { app._selection = []; app._selected = null; return inv('redo'); },
      export:               (fmt, o)     => inv('export_timeline', { fmt, ...o }),
      remove_unsynced:      ()           => inv('remove_unsynced'),
      move_clips:           (moves)      => inv('move_clips', { moves }),
      open_clip:            (ti, ci, reveal) => inv('open_clip', { trackIndex: ti, clipIndex: ci, reveal }),
      clear_all:            ()           => inv('clear_all'),
    };
  },

  addFiles()    { const a = this._api(); if (a) a.add_files(); },
  runSync()     { const a = this._api(); if (a) a.run_sync(); },
  cancelSync()  { const a = this._api(); if (a) a.cancel_sync(); },
  clearAll()    { this._selected = null; this._selection = []; this._userZoomed = false; const a = this._api(); if (a) a.clear_all(); },

  removeTrack(index) { const a = this._api(); if (a) a.remove_track(index); },

  // ── Reordering lanes (drag the ⋮⋮ handle in the sidebar) ─────────────────

  _laneDrag: null,

  _startLaneDrag(e, label, index) {
    if (e.button !== 0 || (this._state && this._state.buttons.cancel)) return;
    e.preventDefault();
    const labels = [...document.querySelectorAll('#sidebar-tracks .track-label')];
    const rows = labels.map(l => document.querySelector(`#tracks-inner .track-row[data-track-index="${l.dataset.trackIndex}"]`));
    // Distance between one track and the next, in each column
    const step = (els) => els.length > 1 && els[0] && els[1]
      ? els[1].getBoundingClientRect().top - els[0].getBoundingClientRect().top
      : (els[0] ? els[0].offsetHeight : 86);
    this._laneDrag = {
      label, index, y0: e.clientY, labels, rows, moved: false, to: index,
      from: labels.indexOf(label), stepL: step(labels), stepR: step(rows),
    };
  },

  // The dragged track follows the cursor in both columns; the others slide
  // out of the way to show where it will land.
  _onLaneDragMove(e) {
    const d = this._laneDrag;
    if (!d) return;
    const dy = e.clientY - d.y0;
    if (!d.moved && Math.abs(dy) < 4) return;
    if (!d.moved) {
      d.moved = true;
      d.label.classList.add('lane-dragging');
      if (d.rows[d.from]) d.rows[d.from].classList.add('row-dragging');
    }
    const n = d.labels.length;
    const to = Math.max(0, Math.min(n - 1, d.from + Math.round(dy / d.stepL)));
    d.to = to;
    d.labels.forEach((l, k) => {
      const row = d.rows[k];
      let shift = 0;
      if (k === d.from) {
        l.style.transform = `translateY(${dy}px)`;
        if (row) row.style.transform = `translateY(${dy * d.stepR / d.stepL}px)`;
        return;
      }
      if (d.from < to && k > d.from && k <= to) shift = -1;
      if (to < d.from && k >= to && k < d.from) shift = 1;
      l.style.transform = shift ? `translateY(${shift * d.stepL}px)` : '';
      if (row) row.style.transform = shift ? `translateY(${shift * d.stepR}px)` : '';
    });
  },

  _onLaneDragEnd() {
    const d = this._laneDrag;
    this._laneDrag = null;
    if (!d || !d.moved) return;
    d.labels.forEach(l => { l.style.transform = ''; l.classList.remove('lane-dragging'); });
    d.rows.forEach(r => { if (r) { r.style.transform = ''; r.classList.remove('row-dragging'); } });
    const to = parseInt(d.labels[d.to].dataset.trackIndex);
    if (d.to !== d.from) {
      this._selection = []; this._selected = null;
      const a = this._api();
      if (a) a.move_track(d.index, to);
    }
  },

  // ── Moving clips by hand ──────────────────────────────────────────────────

  _drag: null,

  _clipStart(clip) {
    return clip.unsynced_offset != null ? clip.unsynced_offset : clip.offset;
  },

  // How far (seconds) these clips can move sideways together before one
  // touches a neighbour on its track: clips stop at the edge, never overlap.
  // Also never before 0.
  _freeRange(clips) {
    const moving = new Set(clips.map(c => c.track_index + '|' + c.clip_index));
    let lo = -Math.min(...clips.map(c => this._clipStart(c))), hi = Infinity;
    const EPS = 1e-6;
    clips.forEach(c => {
      const t = this._state.tracks.find(t => t.index === c.track_index);
      const s0 = this._clipStart(c), s1 = s0 + c.duration;
      (t ? t.clips : []).forEach(o => {
        if (moving.has(o.track_index + '|' + o.clip_index)) return;
        if (o.unsynced_offset != null) return;  // red placeholders move out of the way, they don't block
        const o0 = this._clipStart(o), o1 = o0 + o.duration;
        if (o1 <= s0 + EPS) lo = Math.max(lo, o1 - s0);       // neighbour on the left
        else if (o0 >= s1 - EPS) hi = Math.min(hi, o0 - s1);  // neighbour on the right
        // (already overlapping: leave it be)
      });
    });
    return { lo: Math.min(lo, 0), hi: Math.max(hi, 0) };
  },

  // Whole frames, kept inside the free range
  _clampDelta(delta, range, fps) {
    let d = Math.round(delta * fps) / fps;
    if (d < range.lo) d = Math.ceil(range.lo * fps - 1e-6) / fps;
    if (d > range.hi) d = Math.floor(range.hi * fps + 1e-6) / fps;
    return d;
  },

  // "+3 frames" for small moves, "-32.7 s" for big ones
  _frames(secs) {
    const fps = this._state.timeline.fps;
    const n = Math.round(secs * fps);
    if (Math.abs(n) >= fps * 10) return (secs > 0 ? '+' : '') + secs.toFixed(1) + ' s';
    return (n > 0 ? '+' : '') + n + ' frame' + (Math.abs(n) === 1 ? '' : 's');
  },

  // Drag after a sync. Sideways: move one clip in time (snapped to frames).
  // Up or down: move the selected clips to another lane, keeping their time.
  // The direction is decided by the first few pixels of movement.
  _startDrag(e, el, clip, reselectOnClick) {
    if (!this._state || !this._state.timeline.synced || this._anim) return;
    if (clip.status === 'loading') return;
    this._drag = { el, clip, x0: e.clientX, y0: e.clientY, left0: el.offsetLeft, axis: null, reselectOnClick };
    if (el.closest('.unsynced-panel')) {
      // Placing by hand: where in the clip it was grabbed, as a fraction
      const r = el.getBoundingClientRect();
      this._drag.place = true;
      this._drag.grab = (e.clientX - r.left) / r.width;
    }
    e.preventDefault();
  },

  _onDragMove(e) {
    const d = this._drag;
    if (!d) return;
    const dx = e.clientX - d.x0, dy = e.clientY - d.y0;
    if (d.place) { this._placeMove(e, d, dx, dy); return; }
    if (!d.axis) {
      if (Math.max(Math.abs(dx), Math.abs(dy)) < 5) return;
      d.axis = Math.abs(dy) > Math.abs(dx) ? 'lane' : 'time';
      const sel = [...document.querySelectorAll('#tracks-inner .clip.selected')];
      d.els = sel.length > 1 && sel.includes(d.el) ? sel : [d.el];
      d.lefts = d.els.map(x => x.offsetLeft);
      // Stops at 0 and at neighbours' edges (never overlaps)
      d.range = this._freeRange(d.els.map(x => this._clipFor(x)).filter(Boolean));
      d.els.forEach(x => x.classList.add('dragging', 'drag-' + d.axis));
      if (d.axis === 'lane') {
        // A drop zone under the last track: dropping there makes a new track
        const inner = document.getElementById('tracks-inner');
        const rows = inner.querySelectorAll('.track-row');
        const last = rows[rows.length - 1];
        if (last) {
          d.newRow = document.createElement('div');
          d.newRow.className = 'new-track-drop';
          d.newRow.textContent = '+ New track';
          d.newRow.style.top = (last.offsetTop + last.offsetHeight) + 'px';
          d.newRow.style.height = last.offsetHeight + 'px';
          d.newRow.style.width = inner.scrollWidth + 'px';
          inner.appendChild(d.newRow);
        }
        // Time is locked: fixed guides at every clip's edges, through all tracks
        d.els.forEach(x => {
          this._guide(x.offsetLeft, 'locked');
          this._guide(x.offsetLeft + x.offsetWidth, 'locked');
          const lock = document.createElement('div');
          lock.className = 'clip-lock';
          lock.textContent = '🔒 Time locked';
          x.appendChild(lock);
        });
      } else {
        d.guide = this._guide(d.left0, 'moving');
      }
    }
    if (d.axis === 'time') {
      const ddx = Math.min(d.range.hi * this._pps, Math.max(d.range.lo * this._pps, dx));
      d.el.classList.toggle('at-edge', ddx !== dx);
      d.els.forEach((x, i) => { x.style.left = (d.lefts[i] + ddx) + 'px'; });
      d.guide.style.left = (d.left0 + ddx) + 'px';
      d.guide.dataset.label = this._frames(ddx / this._pps);
      const n = d.els.length;
      this.setStatus(`Moving ${n > 1 ? n + ' clips' : d.clip.name} · ${this._frames(ddx / this._pps)}`, 'default');
      return;
    }
    d.els.forEach(x => { x.style.transform = `translateY(${dy}px)`; });
    document.querySelectorAll('.track-row.drop-target').forEach(r => r.classList.remove('drop-target'));
    const hit = document.elementFromPoint(e.clientX, e.clientY);
    const row = hit && hit.closest('.track-row');
    d.target = row && row.dataset.trackIndex != null && !row.classList.contains('unsynced-row') ? parseInt(row.dataset.trackIndex) : null;
    const n = d.els.length;
    // Below the last track: a new one
    const newHit = d.newRow && d.target == null && e.clientY >= d.newRow.getBoundingClientRect().top;
    if (d.newRow) d.newRow.classList.toggle('active', !!newHit);
    if (newHit) {
      d.target = this._state.tracks.length;
      this.setStatus(`Move ${n} clip${n === 1 ? '' : 's'} to a new track · same time position`, 'default');
    } else if (d.target != null) {
      row.classList.add('drop-target');
      const t = this._state.tracks.find(t => t.index === d.target);
      this.setStatus(`Move ${n} clip${n === 1 ? '' : 's'} to ${t ? t.display_name : 'track'} · same time position`, 'default');
    } else {
      this.setStatus(`Drop on a track to move ${n} clip${n === 1 ? '' : 's'} there`, 'default');
    }
  },

  // Dragging an unsynced clip from the panel: a ghost at timeline scale
  // follows the cursor, with a guide where it would start.
  _placeMove(e, d, dx, dy) {
    if (!d.ghost) {
      if (Math.max(Math.abs(dx), Math.abs(dy)) < 5) return;
      d.ghost = document.createElement('div');
      d.ghost.className = 'place-ghost';
      d.ghost.textContent = d.clip.name;
      d.ghost.style.width = Math.max(4, d.clip.duration * this._pps) + 'px';
      document.body.appendChild(d.ghost);
      d.el.classList.add('dragging');
    }
    const w = Math.max(4, d.clip.duration * this._pps);
    const left = e.clientX - d.grab * w;
    d.ghost.style.left = left + 'px';
    d.ghost.style.top = (e.clientY - 32) + 'px';
    const scroll = document.getElementById('tracks-scroll').getBoundingClientRect();
    const over = e.clientY >= scroll.top && e.clientY <= scroll.bottom && e.clientX >= scroll.left;
    document.querySelectorAll('#tracks-inner .drag-guide').forEach(g => g.remove());
    d.at = null;
    if (over) {
      const inner = document.getElementById('tracks-inner').getBoundingClientRect();
      const fps = this._state.timeline.fps;
      d.at = Math.max(0, Math.round(((left - inner.left) / this._pps) * fps) / fps);
      this._guide(d.at * this._pps, 'moving').dataset.label = this._fmtTc(d.at);
      const t = this._state.tracks.find(t => t.index === d.clip.track_index);
      this.setStatus(`Place ${d.clip.name} at ${this._fmtTc(d.at)} on ${t ? t.display_name : 'its track'}`, 'default');
    } else {
      this.setStatus('Drag up onto the timeline to place this clip manually', 'default');
    }
  },

  _placeEnd(e, d) {
    document.querySelectorAll('#tracks-inner .drag-guide').forEach(g => g.remove());
    if (d.ghost) d.ghost.remove();
    d.el.classList.remove('dragging');
    if (!d.ghost) { if (d.reselectOnClick) this._selectClip(d.el); return; }
    if (d.at == null) { this._render(); return; }
    this._note = `Placed ${d.clip.name} manually at ${this._fmtTc(d.at)}`;
    const a = this._api();
    if (a) a.move_clip(d.clip.track_index, d.clip.clip_index, d.at, false);
  },

  // 83.5 -> "1:23.12" (minutes:seconds.frames)
  _fmtTc(secs) {
    const fps = this._state.timeline.fps;
    const f = Math.round((secs % 1) * fps);
    return Math.floor(secs / 60) + ':' + String(Math.floor(secs % 60)).padStart(2, '0') + '.' + String(Math.min(f, Math.round(fps) - 1)).padStart(2, '0');
  },

  // A vertical line through every track at x (px), like an NLE's edit guide
  _guide(x, kind) {
    const inner = document.getElementById('tracks-inner');
    const g = document.createElement('div');
    g.className = 'drag-guide ' + kind;
    g.style.left = x + 'px';
    g.style.height = inner.scrollHeight + 'px';
    inner.appendChild(g);
    return g;
  },

  _onDragEnd(e) {
    const d = this._drag;
    this._drag = null;
    if (!d) return;
    if (d.place) { this._placeEnd(e, d); return; }
    if (!d.axis) {
      // A plain click on an already-selected clip: select just that one
      if (d.reselectOnClick) this._selectClip(d.el);
      return;
    }
    d.els.forEach(x => { x.classList.remove('dragging', 'drag-' + d.axis, 'at-edge'); x.style.transform = ''; });
    if (d.newRow) d.newRow.remove();
    document.querySelectorAll('#tracks-inner .drag-guide, #tracks-inner .clip-lock').forEach(g => g.remove());
    document.querySelectorAll('.track-row.drop-target').forEach(r => r.classList.remove('drop-target'));
    const a = this._api();
    if (!a) return;
    if (d.axis === 'lane') {
      if (d.target != null) {
        const pairs = this._selectedPairs();
        this._selection = []; this._selected = null;  // indices change with the move
        if (d.target === this._state.tracks.length) this._bornTrack = d.target;  // a new track: grow it in
        a.move_to_lane(pairs, d.target);
      } else this._render();
      return;
    }
    const fps = this._state.timeline.fps;
    if (d.els.length > 1) {
      // Same shift for every clip in the group, whole frames
      const clips = d.els.map(x => this._clipFor(x)).filter(Boolean);
      const delta = this._clampDelta((e.clientX - d.x0) / this._pps, d.range, fps);
      this._note = `Moved ${clips.length} clips · ${this._frames(delta)}`;
      a.move_clips(clips.map(c => [c.track_index, c.clip_index, Math.max(0, this._clipStart(c) + delta)]));
      return;
    }
    const snapped = Math.max(0, this._clipStart(d.clip) + this._clampDelta((e.clientX - d.x0) / this._pps, d.range, fps));
    this._note = `Moved ${d.clip.name} · ${this._frames(snapped - this._clipStart(d.clip))}` +
      (d.clip.unsynced_offset != null ? ' · placed manually' : '');
    a.move_clip(d.clip.track_index, d.clip.clip_index, snapped, false);
  },

  // , and . nudge the selected clip one frame (Shift: ten)
  _nudge(dir, big) {
    const sel = this._selected;
    if (!sel || !this._state || !this._state.timeline.synced) return;
    if (this._selection.length > 1) { this.setStatus('Select one clip to nudge it', 'default'); return; }
    const t = this._state.tracks.find(t => t.index === sel.trackIndex);
    const clip = t && t.clips.find(c => c.clip_index === sel.clipIndex);
    if (!clip || clip.unsynced_offset != null) {
      this.setStatus('Drag an unsynced clip into place first, then nudge it', 'default');
      return;
    }
    const fps = this._state.timeline.fps;
    const frames = dir * (big ? 10 : 1);
    const key = clip.track_index + '|' + clip.clip_index;
    // A run of nudges (key repeat) builds on the last target, not on state
    // that may not have caught up yet
    const run = this._nudged && this._nudged.key === key && Date.now() - this._nudged.at < 1500;
    const base = run ? this._nudged.target : clip.offset;
    const range = this._freeRange([clip]);
    const target = Math.max(0, clip.offset + this._clampDelta(base + frames / fps - clip.offset, range, fps));
    if (Math.abs(target - base) < 1e-6) { this.setStatus(`${clip.name} is against the next clip`, 'default'); return; }
    this._nudged = { key, target, total: (run ? this._nudged.total : 0) + frames, at: Date.now() };
    this._note = `Nudged ${clip.name} · ${this._frames(this._nudged.total / fps)}`;
    const a = this._api();
    if (a) a.move_clip(clip.track_index, clip.clip_index, target, true);
  },

  // ── Export dialog ─────────────────────────────────────────────────────────

  _openExport() {
    if (document.getElementById('btn-export').disabled) return;
    const p = prefs.get();
    document.querySelectorAll('.editor-card').forEach(b => b.classList.toggle('selected', b.dataset.fmt === p.fmt));
    document.getElementById('opt-strip-audio').checked   = p.strip;
    document.getElementById('opt-drop-unsynced').checked = p.drop;
    document.getElementById('export-go').disabled = !p.fmt;
    document.getElementById('export-dialog').style.display = 'flex';
    const sel = document.querySelector('.editor-card.selected') || document.querySelector('.editor-card');
    sel.focus();
  },

  _closeExport() {
    document.getElementById('export-dialog').style.display = 'none';
  },

  _exportOpen() {
    return document.getElementById('export-dialog').style.display !== 'none';
  },

  _pickEditor(fmt) {
    this._setExportOption('fmt', fmt);
    document.querySelectorAll('.editor-card').forEach(b => b.classList.toggle('selected', b.dataset.fmt === fmt));
    document.getElementById('export-go').disabled = false;
  },

  _setExportOption(key, value) {
    const p = prefs.get();
    p[key] = value;
    prefs.set(p);
  },

  // Exports with the editor and options chosen in the dialog
  async doExport() {
    const a = this._api();
    if (!a) return;
    const p = prefs.get();
    if (!p.fmt) return;
    this._closeExport();
    const result = await a.export(p.fmt, { stripCameraAudio: p.strip, dropUnsynced: p.drop });
    if (!result) return;
    if (result.success) {
      this.setStatus('Exported: ' + result.path, 'success');
    } else if (result.error) {
      this.setStatus('Export failed: ' + result.error, 'error');
    }
  },

  // ── Helpers ───────────────────────────────────────────────────────────────

  _rgba(hex, a) {
    const r = parseInt(hex.slice(1, 3), 16);
    const g = parseInt(hex.slice(3, 5), 16);
    const b = parseInt(hex.slice(5, 7), 16);
    return `rgba(${r},${g},${b},${a})`;
  },

  _fmt(secs) {
    const m = Math.floor(secs / 60);
    const s = Math.floor(secs % 60);
    return m + ':' + String(s).padStart(2, '0');
  },

  _statusLabel(clip) {
    switch (clip.status) {
      case 'loading':    return 'loading…';
      case 'ready':      return this._fmt(clip.duration);
      case 'synced':     return (clip.manual ? 'manual · ' : 'synced · ') + this._fmt(clip.duration);
      case 'synced_low': return 'check · ' + this._fmt(clip.duration);
      case 'failed':     return clip.note || 'could not sync';
      case 'new':        return clip.note || 'new · not synced yet';
      default:           return '';
    }
  },

  _clipMeta(clip) {
    const parts = [];
    if (clip.width && clip.height) parts.push(clip.width + '×' + clip.height);
    if (clip.frame_rate)           parts.push(clip.frame_rate + ' fps');
    return parts.join('  ·  ');
  },

  _tooltip(clip) {
    const lines = [clip.name];
    if (clip.duration)      lines.push('Duration: ' + this._fmt(clip.duration));
    if (clip.frame_rate)    lines.push('Frame rate: ' + clip.frame_rate + ' fps');
    if (clip.width && clip.height) lines.push('Resolution: ' + clip.width + 'x' + clip.height);
    if (clip.creation_time) lines.push('Recorded: ' + clip.creation_time.slice(0, 19));
    if (clip.tc_used)       lines.push('Timecode: ' + clip.timecode);
    if (clip.manual)        lines.push('Placed manually');
    else if (clip.confirmed) lines.push('Match: checked by you');
    else if (clip.match)    lines.push('Match: ' + { strong: 'Strong', good: 'Good', check: 'Check this one' }[clip.match]);
    if (clip.note)          lines.push('Note: ' + clip.note);
    return lines.join('\n');
  },
};

// Keyboard shortcuts (not while typing a lane name)
document.addEventListener('keydown', e => {
  if (['INPUT', 'TEXTAREA'].includes(document.activeElement.tagName) || app._exportOpen()) return;
  const a = app._api();
  const ctrl = e.ctrlKey || e.metaKey;
  if (e.key === 'Delete' || e.key === 'Backspace') {
    app._deleteSelected();
  } else if (ctrl && (e.code === 'KeyY' || (e.code === 'KeyZ' && e.shiftKey))) {
    e.preventDefault(); if (a) a.redo();
  } else if (ctrl && e.code === 'KeyZ') {
    e.preventDefault(); if (a) a.undo();
  } else if (!ctrl && !e.altKey && e.code === 'KeyR') {
    app._resetView();
  } else if (!ctrl && (e.code === 'Comma' || e.code === 'Period' || [',', '.', '<', '>'].includes(e.key))) {
    e.preventDefault();
    app._nudge(e.code === 'Comma' || e.key === ',' || e.key === '<' ? -1 : 1, e.shiftKey);
  }
});
document.addEventListener('mousemove', e => { app._onDragMove(e); app._onLaneDragMove(e); app._onMarqueeMove(e); });
document.addEventListener('mouseup',   e => { app._onDragEnd(e); app._onLaneDragEnd(e); app._onMarqueeEnd(e); });
// Click on empty timeline: clear the selection
document.getElementById('tracks-inner').addEventListener('mousedown', e => {
  if (e.button === 0 && !e.target.closest('.clip')) {
    if (!(e.shiftKey || e.ctrlKey || e.metaKey)) {
      document.querySelectorAll('.clip.selected').forEach(c => c.classList.remove('selected'));
      app._selection = []; app._selected = null; app._applyLaneHighlight();
    }
    app._startMarquee(e);
    if (app._filter && app._filter !== 'synced') { app._filter = null; app._goKey = null; app._render(); }
  }
});
document.getElementById('btn-undo').addEventListener('click', () => { const a = app._api(); if (a) a.undo(); });
document.getElementById('btn-redo').addEventListener('click', () => { const a = app._api(); if (a) a.redo(); });

// Buttons (no inline handlers: the app runs with a strict content security policy)
document.getElementById('btn-add').addEventListener('click',    () => app.addFiles());
document.getElementById('btn-sync').addEventListener('click',   () => app.runSync());
document.getElementById('btn-cancel').addEventListener('click', () => app.cancelSync());
document.getElementById('btn-export').addEventListener('click', () => app._openExport());
document.querySelectorAll('.editor-card').forEach(b =>
  b.addEventListener('click', () => app._pickEditor(b.dataset.fmt)));
document.getElementById('opt-strip-audio').addEventListener('change',   e => app._setExportOption('strip', e.target.checked));
document.getElementById('opt-drop-unsynced').addEventListener('change', e => app._setExportOption('drop', e.target.checked));
document.getElementById('export-cancel').addEventListener('click', () => app._closeExport());
document.getElementById('export-go').addEventListener('click', () => app.doExport());
// Click outside the dialog closes it
document.getElementById('export-dialog').addEventListener('mousedown', e => {
  if (e.target.id === 'export-dialog') app._closeExport();
});
document.addEventListener('keydown', e => {
  if (!app._exportOpen()) return;
  if (e.key === 'Escape') app._closeExport();
  if (e.key === 'Enter' && !document.getElementById('export-go').disabled) { e.preventDefault(); app.doExport(); }
});
document.getElementById('btn-clear').addEventListener('click',  () => app.clearAll());
document.getElementById('drop-target').addEventListener('click', () => app.addFiles());
document.getElementById('btn-licences').addEventListener('click', () => {
  if (window.__TAURI__) window.__TAURI__.core.invoke('open_notices');
});

// Right-click menu
document.getElementById('ctx-menu').addEventListener('click', e => {
  const action = e.target.dataset && e.target.dataset.action;
  if (!action || e.target.disabled) return;
  app._hideMenu();
  if (action === 'remove-clip') app._deleteSelected();
  if ((action === 'open-clip' || action === 'reveal-clip') && app._selected) {
    const a = app._api();
    if (a) a.open_clip(app._selected.trackIndex, app._selected.clipIndex, action === 'reveal-clip');
  }
  if (action === 'remove-unsynced') { const a = app._api(); if (a) a.remove_unsynced().then(() => setTimeout(() => app._resetView(), 80)); }
});
document.addEventListener('mousedown', e => {
  if (!e.target.closest('#ctx-menu')) app._hideMenu();
});
document.addEventListener('keydown', e => { if (e.key === 'Escape') app._hideMenu(); });
// Any click or key skips the post-sync reveal
document.addEventListener('mousedown', () => app._finishAnim(), true);
document.addEventListener('keydown',   () => app._finishAnim(), true);
document.getElementById('tracks-scroll').addEventListener('scroll', () => app._hideMenu());
window.addEventListener('blur', () => app._hideMenu());
// No browser menu (Reload / Inspect) anywhere except text fields
document.addEventListener('contextmenu', e => {
  if (!['INPUT', 'TEXTAREA'].includes(e.target.tagName)) e.preventDefault();
});

// Bridge to Rust: state/status events, native file drag and drop
(async () => {
  const T = window.__TAURI__;
  if (!T) return;
  const prefsReady = prefs.load();  // don't miss state events while this loads
  await T.event.listen('state',  e => app.update(e.payload));
  await T.event.listen('status', e => app.setStatus(e.payload.text, e.payload.type));
  await T.event.listen('tauri://drag-enter', () => document.body.classList.add('drag-active'));
  await T.event.listen('tauri://drag-leave', () => document.body.classList.remove('drag-active'));
  const focusWindow = () => { if (!document.hasFocus()) T.core.invoke('focus_window').catch(() => {}); };
  // WebView2 ignores the wheel until the window has focus (see focus_window).
  // Only on drop: focusing on hover would cover the Add media / Export dialogs.
  await T.event.listen('tauri://drag-drop',  e => {
    document.body.classList.remove('drag-active');
    focusWindow();
    const paths = (e.payload && e.payload.paths) || [];
    if (paths.length > 0) { const a = app._api(); if (a) a.add_files_from_paths(paths); }
  });
  await prefsReady;
  app.update(await T.core.invoke('get_state'));
  try {
    document.getElementById('about-version').textContent = 'ShotSynq ' + await T.core.invoke('app_version');
  } catch (e) { /* ignore */ }
})();

// On resize: refit if not zoomed, otherwise rerender at current zoom
window.addEventListener('resize', () => {
  if (!app._state) return;
  if (app._userZoomed) app._rerenderForZoom();
  else app._render();
});

// Init zoom controls once DOM is ready
app._initWheelZoom();
app._initZoomBar();
