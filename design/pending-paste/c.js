{
  ph: function () { return '<span class="ghost">[pasting<span class="dots"></span>]</span>'; },
  held: function (t, w) { return t.replace(/&/g,'&amp;') + (w === 'term' ? '<span class="cur"></span>' : '<span class="icur"></span>'); },
  tstat: function () { return '<span class="pst" data-show-state="copying" role="status"><span class="spin"></span>Pasting from your other device<button class="x" aria-label="Cancel paste">Cancel <kbd>esc</kbd></button></span>'; }
}
