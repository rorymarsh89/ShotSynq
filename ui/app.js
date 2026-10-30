'use strict';

// How long the "clips sliding into place" reveal after a sync takes.
// Purely presentational: the sync itself is already finished. 0 = off.
const SYNC_ANIM_MS  = 5000;
const SYNC_MOVE_MS  = 260;   // each clip's snap into place
const SYNC_EASE     = 'cubic-bezier(.3,1.45,.55,1)';  // fast, slight overshoot

const EDITOR_NAMES = { resolve: 'Resolve', premiere: 'Premiere', fcpxml: 'Final Cut' };
const DEFAULT_PREFS = { fmt: null, strip: false, drop: false };  // no editor until the user picks one

// Export choices are remembered between sessions (best effort).
const prefs = {
  _key: 'shotsynq.export',
  get() {
    try { return Object.assign({}, DEFAULT_PREFS, JSON.parse(localStorage.getItem(this._key) || '{}')); }
    catch (e) { return Object.assign({}, DEFAULT_PREFS); }
  },
  set(v) { try { localStorage.setItem(this._key, JSON.stringify(v)); } catch (e) { /* ignore */ } },
};

const app = {
  _state:        null,
  _filter:       null,  // null | 'synced' | 'review' | 'unplaced'
  _selected:     null,  // { trackIndex, clipIndex }
  _pps:          100,   // pixels per second
  _userZoomed:   false, // true once user has manually zoomed

  // ── State update (pushed from Rust) ──────────────────────────────────────

  update(state) {
    const prev = this._state;
    const justSynced = prev && prev.buttons.cancel && !state.buttons.cancel &&
      state.timeline.synced && ['success', 'warning'].includes(state.status.type);
    this._finishAnim();
    const before = justSynced && this._animEnabled() ? this._capturePositions() : null;
    this._state = state;
    this._render();
    if (before) this._animateSync(before, state.status);
  },

  // ── Post-sync reveal ──────────────────────────────────────────────────────

  _anim: null,  // { timers: [], items: [], status }

  _animEnabled() {
    return SYNC_ANIM_MS > 0 &&
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
    items.sort((a, b) => a.left - b.left);

    // Start everything where it was before the sync, still in "waiting" style.
    items.forEach(it => {
      it.el.style.transition = 'none';
      it.el.style.left  = it.from.left + 'px';
      it.el.style.width = it.from.width + 'px';
      it.el.className = it.from.cls.replace(/\bselected\b/, '') + ' anim-waiting';
    });
    void document.body.offsetHeight;  // commit start positions

    const n = items.length;
    const step = n > 1 ? Math.max(0, SYNC_ANIM_MS - SYNC_MOVE_MS) / (n - 1) : 0;
    const anim = { timers: [], items, status: finalStatus };
    this._anim = anim;
    let placed = 0;
    this.setStatus(`Placing clips… 0/${n}`, 'loading');

    items.forEach((it, i) => {
      anim.timers.push(setTimeout(() => {
        it.el.className = it.cls + ' lifting';
        it.el.style.transition =
          `left ${SYNC_MOVE_MS}ms ${SYNC_EASE}, width ${SYNC_MOVE_MS}ms ${SYNC_EASE}, ` +
          'transform 90ms ease-out, box-shadow 90ms ease-out';
        it.el.style.left  = it.left + 'px';
        it.el.style.width = it.width + 'px';
        anim.timers.push(setTimeout(() => {
          it.el.classList.remove('lifting');
          it.el.classList.add('snapped');   // landing flash
          anim.timers.push(setTimeout(() => it.el.classList.remove('snapped'), 320));
          placed++;
          if (placed < n) this.setStatus(`Placing clips… ${placed}/${n}`, 'loading');
          else this._finishAnim();
        }, SYNC_MOVE_MS));
      }, i * step));
    });
  },

  _finishAnim() {
    const a = this._anim;
    if (!a) return;
    this._anim = null;
    a.timers.forEach(clearTimeout);
    a.items.forEach(it => {
      it.el.style.transition = '';
      it.el.style.left  = it.left + 'px';
      it.el.style.width = it.width + 'px';
      it.el.className = it.cls + (it.el.classList.contains('snapped') ? ' snapped' : '');
    });
    this.setStatus(a.status.text, a.status.type);
    // let the last landing flash play out
    setTimeout(() => document.querySelectorAll('.clip.snapped').forEach(e => e.classList.remove('snapped')), 320);
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
    document.getElementById('btn-sync').disabled   = !s.buttons.sync;
    document.getElementById('btn-export').disabled      = !s.buttons.export;
    document.getElementById('btn-export-menu').disabled = !s.buttons.export;
    const fmt = prefs.get().fmt;
    document.getElementById('btn-export').textContent = '\u2191 Export' + (fmt ? ' to ' + EDITOR_NAMES[fmt] : '');
    if (!s.buttons.export) this._hideExportMenu();
    // One bright button: the next step
    const syncNext = s.buttons.sync && !s.timeline.synced;
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
    this._renderTray(s.tracks, s.timeline);
    this._applyLaneHighlight();
    this._drawRuler(s.timeline.total_duration);
    this._updateZoomBar();
  },

  _renderSidebar(tracks) {
    const container = document.getElementById('sidebar-tracks');
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
      label.style.height = rowH + 'px';

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
      meta.textContent = track.meta;

      info.appendChild(nameInput);
      info.appendChild(meta);

      const removeBtn = document.createElement('button');
      removeBtn.className   = 'track-remove';
      removeBtn.textContent = '✕';
      removeBtn.title       = 'Remove track';
      removeBtn.addEventListener('click', e => { e.stopPropagation(); const a = app._api(); if (a) a.remove_track(track.index); });

      label.appendChild(dot);
      label.appendChild(info);
      label.appendChild(removeBtn);
      container.appendChild(label);
    });
  },

  _renderTimeline(tracks, timeline) {
    const inner = document.getElementById('tracks-inner');

    // Remove old track rows, keep the drop hint
    inner.querySelectorAll('.track-row').forEach(el => el.remove());

    if (tracks.length === 0) return;

    const pps   = this._pps;
    const GAP   = 3;
    const MIN_W = 36;
    const vpW   = document.getElementById('tracks-scroll').clientWidth;
    let innerW  = Math.max(timeline.total_duration * pps + 20, vpW);
    inner.style.width = innerW + 'px';

    tracks.forEach(track => {
      const row = document.createElement('div');
      row.className = 'track-row ' + (track.is_video ? 'cam-row' : 'aud-row');
      row.dataset.trackIndex = track.index;
      row.style.setProperty('--lane', track.color);
      inner.appendChild(row);

      // Positions first, so each clip can be clipped to where the next one
      // starts. Short clips at low zoom otherwise spill over their neighbours.
      let runningPx = 0;
      const onTimeline = timeline.synced ? track.clips.filter(c => c.status !== 'failed') : track.clips;
      const items = onTimeline.map(clip => {
        let leftPx, widthPx;
        if (timeline.synced) {
          leftPx  = Math.round(clip.offset * pps);
          widthPx = Math.max(Math.round(clip.duration * pps) - GAP, 2);
        } else {
          widthPx = Math.max(Math.round(clip.duration * pps) - GAP, MIN_W);
          leftPx  = runningPx;
          runningPx += widthPx + GAP;
        }
        return { clip, leftPx, widthPx };
      });
      const byLeft = [...items].sort((a, b) => a.leftPx - b.leftPx);
      for (let k = 0; k < byLeft.length - 1; k++) {
        const room = byLeft[k + 1].leftPx - byLeft[k].leftPx - 1;
        if (room > 0) byLeft[k].widthPx = Math.max(2, Math.min(byLeft[k].widthPx, room));
      }

      items.forEach(({ clip, leftPx, widthPx }) => {
        const right = leftPx + widthPx + 20;
        if (right > innerW) { innerW = right; inner.style.width = innerW + 'px'; }

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

        // Waveform canvas
        const wc = document.createElement('canvas');
        wc.className = 'clip-wave';
        el.appendChild(wc);

        // Label
        const lbl = document.createElement('div');
        lbl.className = 'clip-label';

        const nameEl   = document.createElement('div');
        nameEl.className = 'clip-name';
        nameEl.textContent = clip.name;

        const statusEl = document.createElement('div');
        statusEl.className = 'clip-status';
        statusEl.textContent = this._statusLabel(clip);

        lbl.appendChild(nameEl);
        lbl.appendChild(statusEl);
        el.appendChild(lbl);

        el.addEventListener('click', () => this._selectClip(el));
        el.addEventListener('contextmenu', e => {
          e.preventDefault();
          this._selectClip(el);
          this._showMenu(e.clientX, e.clientY, clip);
        });
        row.appendChild(el);

        // Draw waveform once layout is settled
        if (clip.waveform && clip.waveform.length > 0) {
          requestAnimationFrame(() => this._drawWave(wc, clip.waveform, track.color));
        }
      });
    });

    // Restore selection highlight if still present
    if (this._selected) {
      const { trackIndex: ti, clipIndex: ci } = this._selected;
      const el = inner.querySelector(`[data-track-index="${ti}"][data-clip-index="${ci}"]`);
      if (el) el.classList.add('selected');
    }
  },

  _drawWave(canvas, peaks, color) {
    const parent = canvas.parentElement;
    const W = parent.clientWidth;
    const H = parent.clientHeight;
    if (!W || !H) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width  = W * dpr;
    canvas.height = H * dpr;
    canvas.style.width  = W + 'px';
    canvas.style.height = H + 'px';
    const ctx = canvas.getContext('2d');
    ctx.scale(dpr, dpr);
    ctx.strokeStyle = color;
    ctx.lineWidth   = 1;
    const mid  = H / 2;
    const step = W / peaks.length;
    ctx.beginPath();
    for (let i = 0; i < peaks.length; i++) {
      const x = i * step;
      const a = peaks[i] * mid * 0.85;
      ctx.moveTo(x, mid - a);
      ctx.lineTo(x, mid + a);
    }
    ctx.stroke();
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
    for (let i = 0; i < nTicks; i++) {
      const t = i * subIv;
      const x = t * pps;
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
    for (let i = 0; i <= nLabels; i++) {
      const t  = i * iv.major;
      const x  = t * pps;
      if (x > W + 2) break;
      const lbl = iv.fmt(t);
      const tw  = ctx.measureText(lbl).width;
      const lx  = Math.max(2, Math.min(x - tw / 2, W - tw - 2));
      if (lx > lastX + tw + 14) { ctx.fillText(lbl, lx, 6); lastX = lx; }
    }
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

    scroll.addEventListener('scroll', () => this._updateZoomBar());

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
      if (e.ctrlKey || e.metaKey) {
        const factor = e.deltaY < 0 ? 1.12 : 1 / 1.12;
        this._zoom(factor, e.offsetX);
      } else {
        scroll.scrollLeft += (Math.abs(e.deltaY) > Math.abs(e.deltaX) ? e.deltaY : e.deltaX) * 1.5;
        this._updateZoomBar();
      }
    }, { passive: false });
  },

  // ── Clip selection ────────────────────────────────────────────────────────

  _selectClip(el) {
    document.querySelectorAll('.clip.selected, .tray-clip.selected').forEach(c => c.classList.remove('selected'));
    el.classList.add('selected');
    this._selected = {
      trackIndex: parseInt(el.dataset.trackIndex),
      clipIndex:  parseInt(el.dataset.clipIndex),
      tray:       el.classList.contains('tray-clip'),
    };
    this._applyLaneHighlight();
  },

  _showMenu(x, y, clip) {
    const menu = document.getElementById('ctx-menu');
    const anyFailed = this._state && this._state.tracks.some(t => t.clips.some(c => c.status === 'failed'));
    menu.querySelector('[data-action="remove-unsynced"]').disabled = !anyFailed;
    // Native hover tooltips would sit on top of the menu; park them meanwhile
    document.querySelectorAll('.clip[title], .tray-clip[title]').forEach(e => {
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
    return { synced: 'synced', review: 'synced_low', unplaced: 'failed' }[this._filter] === clip.status;
  },

  _setFilter(f) {
    this._filter = this._filter === f ? null : f;
    this._render();
  },

  _renderFooter(s) {
    const done = s.timeline.synced && !s.buttons.cancel;
    const chips = document.getElementById('status-chips');
    const stats = document.getElementById('footer-stats');
    document.getElementById('zoom-hint').style.display = done ? 'none' : '';
    chips.style.display = done ? 'flex' : 'none';
    stats.style.display = done ? '' : 'none';
    if (!done) { this._filter = null; return; }

    const all = s.tracks.flatMap(t => t.clips);
    const n = st => all.filter(c => c.status === st).length;
    const counts = [
      ['synced',   'ok',       n('synced'),     'synced'],
      ['review',   'review',   n('synced_low'), 'to review'],
      ['unplaced', 'unplaced', n('failed'),     'unplaced'],
    ];
    chips.innerHTML = '';
    counts.forEach(([key, cls, count, label]) => {
      if (count === 0 && key !== 'synced') return;
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'chip ' + cls + (this._filter === key ? ' active' : '');
      b.innerHTML = '<i></i>';
      b.appendChild(document.createTextNode(count + ' ' + label));
      b.title = this._filter === key ? 'Show all clips' : 'Highlight these clips';
      b.addEventListener('click', () => this._setFilter(key));
      chips.appendChild(b);
    });

    const nClips = all.length;
    stats.textContent = `${nClips} clips · ${this._fmtLong(s.timeline.footage)} footage`;
  },

  _fmtLong(secs) {
    if (secs < 60) return Math.round(secs) + 's';
    const h = Math.floor(secs / 3600), m = Math.round((secs % 3600) / 60);
    return h > 0 ? `${h}h ${String(m).padStart(2, '0')}m` : `${Math.max(m, secs > 0 ? 1 : 0)}m`;
  },

  // ── Unplaced tray ─────────────────────────────────────────────────────────

  _renderTray(tracks, timeline) {
    const tray = document.getElementById('tray');
    const list = document.getElementById('tray-clips');
    const failed = timeline.synced
      ? tracks.flatMap(t => t.clips.filter(c => c.status === 'failed').map(c => ({ c, t })))
      : [];
    tray.style.display = failed.length ? 'flex' : 'none';
    tray.classList.toggle('dimmed', !!this._filter && this._filter !== 'unplaced');
    document.getElementById('tray-count').textContent = failed.length;
    list.innerHTML = '';
    failed.forEach(({ c, t }) => {
      const el = document.createElement('div');
      el.className = 'tray-clip';
      el.dataset.trackIndex = c.track_index;
      el.dataset.clipIndex  = c.clip_index;
      el.title = this._tooltip(c) + '\n' + t.display_name;
      const dot = document.createElement('span');
      dot.className = 'tray-dot';
      dot.style.background = t.color;
      const name = document.createElement('span');
      name.textContent = c.name;
      const dur = document.createElement('span');
      dur.className = 'tray-dur';
      dur.textContent = this._fmt(c.duration);
      el.append(dot, name, dur);
      el.addEventListener('click', () => this._selectClip(el));
      el.addEventListener('contextmenu', e => {
        e.preventDefault();
        this._selectClip(el);
        this._showMenu(e.clientX, e.clientY, c);
      });
      if (this._selected && this._selected.trackIndex === c.track_index && this._selected.clipIndex === c.clip_index) {
        el.classList.add('selected');
      }
      list.appendChild(el);
    });
  },

  // An unplaced clip has no timeline position, so show which lane it belongs to.
  _applyLaneHighlight() {
    document.querySelectorAll('.lane-hl').forEach(e => e.classList.remove('lane-hl'));
    const sel = this._selected;
    if (!sel || !sel.tray) return;
    document.querySelectorAll(`.track-row[data-track-index="${sel.trackIndex}"], .track-label[data-track-index="${sel.trackIndex}"]`)
      .forEach(e => e.classList.add('lane-hl'));
  },

  _deleteSelected() {
    if (!this._selected) return;
    const a = this._api();
    if (a) a.delete_clip(this._selected.trackIndex, this._selected.clipIndex);
    this._selected = null;
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
      export:               (fmt, o)     => inv('export_timeline', { fmt, ...o }),
      remove_unsynced:      ()           => inv('remove_unsynced'),
      open_clip:            (ti, ci, reveal) => inv('open_clip', { trackIndex: ti, clipIndex: ci, reveal }),
      clear_all:            ()           => inv('clear_all'),
    };
  },

  addFiles()    { const a = this._api(); if (a) a.add_files(); },
  runSync()     { const a = this._api(); if (a) a.run_sync(); },
  cancelSync()  { const a = this._api(); if (a) a.cancel_sync(); },
  clearAll()    { this._selected = null; this._userZoomed = false; const a = this._api(); if (a) a.clear_all(); },

  removeTrack(index) { const a = this._api(); if (a) a.remove_track(index); },

  // ── Export dropdown ───────────────────────────────────────────────────────

  _exportAfterPick: false,  // menu was opened by the main button with no editor chosen

  _showExportMenu(exportAfterPick) {
    const p = prefs.get();
    const menu = document.getElementById('export-menu');
    menu.querySelectorAll('.dd-item').forEach(b => b.classList.toggle('selected', b.dataset.fmt === p.fmt));
    document.getElementById('opt-strip-audio').checked   = p.strip;
    document.getElementById('opt-drop-unsynced').checked = p.drop;
    this._exportAfterPick = !!exportAfterPick;
    menu.style.display = 'flex';
  },

  _hideExportMenu() {
    document.getElementById('export-menu').style.display = 'none';
    this._exportAfterPick = false;
  },

  _toggleExportMenu() {
    const open = document.getElementById('export-menu').style.display !== 'none';
    if (open) this._hideExportMenu(); else this._showExportMenu(false);
  },

  _pickEditor(fmt) {
    const p = prefs.get();
    p.fmt = fmt;
    prefs.set(p);
    const go = this._exportAfterPick;
    this._hideExportMenu();
    this._render();
    if (go) this.doExport();
  },

  _setExportOption(key, value) {
    const p = prefs.get();
    p[key] = value;
    prefs.set(p);
  },

  // Exports with the remembered editor and options; asks for an editor first time
  async doExport() {
    const a = this._api();
    if (!a) return;
    const p = prefs.get();
    if (!p.fmt) { this._showExportMenu(true); return; }
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
      case 'synced':     return 'synced · ' + this._fmt(clip.duration);
      case 'synced_low': return 'low confidence · ' + this._fmt(clip.duration);
      case 'failed':     return 'could not sync';
      default:           return '';
    }
  },

  _clipMeta(clip) {
    const parts = [];
    if (clip.width && clip.height) parts.push(clip.width + '×' + clip.height);
    if (clip.frame_rate)           parts.push(clip.frame_rate + ' fps');
    if (clip.confidence > 0)       parts.push('conf ' + clip.confidence.toFixed(2));
    return parts.join('  ·  ');
  },

  _tooltip(clip) {
    const lines = [clip.name];
    if (clip.duration)      lines.push('Duration: ' + this._fmt(clip.duration));
    if (clip.frame_rate)    lines.push('Frame rate: ' + clip.frame_rate + ' fps');
    if (clip.width && clip.height) lines.push('Resolution: ' + clip.width + 'x' + clip.height);
    if (clip.creation_time) lines.push('Recorded: ' + clip.creation_time.slice(0, 19));
    if (clip.timecode && clip.timecode !== '00:00:00:00') lines.push('Timecode: ' + clip.timecode);
    if (clip.confidence > 0) lines.push('Confidence: ' + clip.confidence.toFixed(4));
    return lines.join('\n');
  },
};

// Keyboard shortcuts
document.addEventListener('keydown', e => {
  if ((e.key === 'Delete' || e.key === 'Backspace') &&
      !['INPUT', 'TEXTAREA'].includes(document.activeElement.tagName)) {
    app._deleteSelected();
  }
});

// Buttons (no inline handlers: the app runs with a strict content security policy)
document.getElementById('btn-add').addEventListener('click',    () => app.addFiles());
document.getElementById('btn-sync').addEventListener('click',   () => app.runSync());
document.getElementById('btn-cancel').addEventListener('click', () => app.cancelSync());
document.getElementById('btn-export').addEventListener('click',      () => app.doExport());
document.getElementById('btn-export-menu').addEventListener('click', () => app._toggleExportMenu());
document.querySelectorAll('#export-menu .dd-item').forEach(b =>
  b.addEventListener('click', () => app._pickEditor(b.dataset.fmt)));
document.getElementById('opt-strip-audio').addEventListener('change',   e => app._setExportOption('strip', e.target.checked));
document.getElementById('opt-drop-unsynced').addEventListener('change', e => app._setExportOption('drop', e.target.checked));
document.addEventListener('mousedown', e => {
  if (!e.target.closest('#export-split')) app._hideExportMenu();
});
document.addEventListener('keydown', e => { if (e.key === 'Escape') app._hideExportMenu(); });
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
  if (action === 'remove-unsynced') { const a = app._api(); if (a) a.remove_unsynced(); }
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
  await T.event.listen('state',  e => app.update(e.payload));
  await T.event.listen('status', e => app.setStatus(e.payload.text, e.payload.type));
  await T.event.listen('tauri://drag-enter', () => document.body.classList.add('drag-active'));
  await T.event.listen('tauri://drag-leave', () => document.body.classList.remove('drag-active'));
  await T.event.listen('tauri://drag-drop',  e => {
    document.body.classList.remove('drag-active');
    const paths = (e.payload && e.payload.paths) || [];
    if (paths.length > 0) { const a = app._api(); if (a) a.add_files_from_paths(paths); }
  });
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
