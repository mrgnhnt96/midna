{
  ph: function () { return '<span class="chip" data-show-state="copying slow" role="status"><span class="spin"></span>Pasting…<span class="secs" data-show-state="slow">4s</span><span class="esc">esc</span><button class="x" title="Cancel paste" aria-label="Cancel paste">✕</button></span><span class="chip fail" data-show-state="gaveup" role="status">Nothing arrived · not pasted</span>'; },
  held: function (t, w) { return t.replace(/&/g,'&amp;') + (w === 'term' ? '<span class="cur"></span>' : '<span class="icur"></span>'); },
  chint: function () { return '<span data-show-state="copying slow">↩ sends once the paste is in · esc cancels the paste</span><span data-show-state="landed cancelled gaveup">↩ send to claude · esc cancel</span>'; }
}
