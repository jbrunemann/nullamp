// A tiny stand-in for the deck: the title follows the track under the playhead.
(function () {
  var tracks = [
    { at: 0, title: "Night Shift (Extended Mix)", exact: true },
    { at: 150, title: "Glass Harbour", exact: true },
    { at: 560, title: "Low Tide (Dub)", exact: false },
    { at: 900, title: "Parallel Lines (Remix)", exact: true },
    { at: 1290, title: "Sunday Static", exact: false },
    { at: 1560, title: "Closing Hours", exact: true }
  ];
  var total = 1860;
  var tl = document.getElementById("demo-tl");
  if (!tl) return;
  var played = document.getElementById("demo-played");
  var head = document.getElementById("demo-head");
  var timeEl = document.getElementById("demo-time");
  var titleEl = document.getElementById("demo-title");
  var nEl = document.getElementById("demo-n");
  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  var wave = document.createElement("div");
  wave.className = "wave";
  for (var i = 0; i < 120; i++) {
    var b = document.createElement("i");
    var h = 25 + 55 * Math.abs(Math.sin(i * 0.37) * Math.cos(i * 0.11)) + 12 * Math.abs(Math.sin(i * 1.9));
    b.style.height = Math.min(100, h) + "%";
    wave.appendChild(b);
  }
  tl.insertBefore(wave, played);

  var segs = tracks.map(function (t, k) {
    var s = document.createElement("div");
    var end = k + 1 < tracks.length ? tracks[k + 1].at : total;
    s.className = "seg" + (t.exact ? "" : " est");
    s.style.left = (t.at / total * 100) + "%";
    s.style.width = ((end - t.at) / total * 100) + "%";
    tl.appendChild(s);
    return s;
  });
  tl.appendChild(head);

  function fmt(sec) {
    var m = Math.floor(sec / 60), s = Math.floor(sec % 60);
    return m + ":" + (s < 10 ? "0" : "") + s;
  }
  function current(sec) {
    var k = 0;
    for (var i = 0; i < tracks.length; i++) if (sec >= tracks[i].at) k = i;
    return k;
  }
  var shown = -1;
  function render(sec) {
    var k = current(sec);
    head.style.left = "calc(" + (sec / total * 100) + "% - 1px)";
    timeEl.textContent = fmt(sec);
    tl.setAttribute("aria-valuenow", Math.round(sec / total * 100));
    if (k !== shown) {
      if (shown >= 0) segs[shown].classList.remove("now");
      segs[k].classList.add("now");
      titleEl.textContent = tracks[k].title + (tracks[k].exact ? "" : "  ≈");
      nEl.textContent = k + 1;
      shown = k;
    }
  }

  var pos = 40, last = null, timer = null;
  function tick(now) {
    if (last !== null) pos += (now - last) / 1000 * 12;
    last = now;
    if (pos >= total) pos = 0;
    render(pos);
    timer = requestAnimationFrame(tick);
  }
  function seekTo(clientX) {
    var r = tl.getBoundingClientRect();
    pos = Math.max(0, Math.min(1, (clientX - r.left) / r.width)) * (total - 1);
    render(pos);
  }
  tl.addEventListener("click", function (e) { seekTo(e.clientX); });
  tl.addEventListener("keydown", function (e) {
    if (e.key === "ArrowRight") pos = Math.min(total - 1, pos + 30);
    else if (e.key === "ArrowLeft") pos = Math.max(0, pos - 30);
    else return;
    e.preventDefault();
    render(pos);
  });

  render(pos);
  // Run only while visible, and not at all when the visitor prefers less motion.
  if (!reduce && "IntersectionObserver" in window) {
    new IntersectionObserver(function (entries) {
      if (entries[0].isIntersecting) {
        last = null;
        if (!timer) timer = requestAnimationFrame(tick);
      } else if (timer) {
        cancelAnimationFrame(timer);
        timer = null;
      }
    }).observe(tl);
  }
})();
