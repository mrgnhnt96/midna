{
  ph: function () { return '<span class="skel" role="status" aria-label="Paste on its way"><span class="tag"><span class="spin"></span>Copying from your other device <kbd>esc</kbd></span></span>'; },
  held: function (t, w) { return '<span class="held" title="Sent after the paste">' + t.replace(/&/g,'&amp;') + '</span>' + (w === 'term' ? '<span class="cur"></span>' : '<span class="icur"></span>'); }
}
