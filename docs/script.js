/* EternalMonitor site. No dependencies, no analytics. */
(function () {
  'use strict';

  var reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var REPO = 'sidebandstudio/EternalMonitor';
  var FEEDBACK_TO = 'hello@sideband.studio';

  /* --- Nav: hairline once the page scrolls ------------------------------ */
  var nav = document.querySelector('.nav');
  if (nav) {
    var ticking = false;
    var updateNav = function () {
      nav.classList.toggle('scrolled', window.scrollY > 4);
      ticking = false;
    };
    window.addEventListener('scroll', function () {
      if (!ticking) { ticking = true; window.requestAnimationFrame(updateNav); }
    }, { passive: true });
    updateNav();
  }

  /* --- Mac visitors: one line, dismissable ------------------------------ */
  // iPadOS reports 'MacIntel' too, so require a non-touch device.
  var isMac = /Mac/.test(navigator.platform) && navigator.maxTouchPoints <= 1;
  if (isMac && nav && sessionStorage.getItem('mac-notice') !== 'off') {
    var notice = document.createElement('aside');
    notice.className = 'mac-notice';
    notice.setAttribute('aria-label', 'Note for Mac visitors');
    notice.innerHTML =
      '<div class="container mac-notice-inner">' +
      '<p><strong>On a Mac?</strong>EternalMonitor streams from Windows PCs. macOS already has ' +
      '<a href="https://support.apple.com/en-us/102386" target="_blank" rel="noopener">Sidecar</a> for this.</p>' +
      '<button type="button" class="mac-notice-close" aria-label="Dismiss">&times;</button></div>';
    notice.querySelector('.mac-notice-close').addEventListener('click', function () {
      sessionStorage.setItem('mac-notice', 'off');
      notice.remove();
    });
    document.body.appendChild(notice);
  }

  /* --- Scroll reveal ----------------------------------------------------- */
  var reveals = document.querySelectorAll('.reveal');
  if (reveals.length && 'IntersectionObserver' in window && !reducedMotion) {
    var revealer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('visible');
          revealer.unobserve(entry.target);
        }
      });
    }, { threshold: 0.12, rootMargin: '0px 0px -32px 0px' });
    reveals.forEach(function (el) { revealer.observe(el); });
  } else {
    reveals.forEach(function (el) { el.classList.add('visible'); });
  }

  /* --- Hero film ------------------------------------------------------------
     The short loop plays silently when it scrolls into view, unless the
     visitor prefers reduced motion. "Full version" swaps in the longer cut
     with native controls. */
  var film = document.getElementById('film');
  var filmFrame = film && film.closest('.film-frame');
  var filmStage = film && film.closest('.film-stage');
  if (film && filmFrame) {
    var playBtn = document.getElementById('film-play');
    var fullBtn = document.getElementById('film-full');
    var loop = { mp4: film.getAttribute('data-loop-mp4'), webm: film.getAttribute('data-loop-webm') };
    var full = { mp4: film.getAttribute('data-full-mp4'), webm: film.getAttribute('data-full-webm') };
    var wantsPlay = !reducedMotion;
    var inView = false;

    var setSources = function (src) {
      while (film.firstChild) film.removeChild(film.firstChild);
      // H.264 first: hardware decoding everywhere, including iPad and Safari.
      [['mp4', 'video/mp4'], ['webm', 'video/webm']].forEach(function (pair) {
        if (!src[pair[0]]) return;
        var s = document.createElement('source');
        s.src = src[pair[0]];
        s.type = pair[1];
        film.appendChild(s);
      });
      film.load();
    };
    var setPlayLabel = function () {
      if (!playBtn) return;
      var playing = !film.paused && !film.ended;
      playBtn.querySelector('.label').textContent = playing ? 'Pause' : 'Play';
      playBtn.setAttribute('aria-pressed', playing ? 'true' : 'false');
      // SVG elements have no `hidden` property, so toggle the attribute.
      playBtn.querySelector('.ico-play').toggleAttribute('hidden', playing);
      playBtn.querySelector('.ico-pause').toggleAttribute('hidden', !playing);
    };
    var tryPlay = function () {
      var p = film.play();
      if (p && p.catch) p.catch(function () { wantsPlay = false; setPlayLabel(); });
    };

    setSources(loop);
    film.addEventListener('play', setPlayLabel);
    film.addEventListener('pause', setPlayLabel);
    setPlayLabel();

    if ('IntersectionObserver' in window) {
      new IntersectionObserver(function (entries) {
        inView = entries[0].isIntersecting;
        if (film.controls) return;
        if (inView && wantsPlay) tryPlay(); else if (!inView) film.pause();
      }, { threshold: 0.35 }).observe(filmFrame);
    } else if (wantsPlay) {
      tryPlay();
    }

    if (playBtn) {
      playBtn.addEventListener('click', function () {
        if (film.paused) { wantsPlay = true; tryPlay(); } else { wantsPlay = false; film.pause(); }
      });
    }
    if (fullBtn && full.mp4) {
      fullBtn.addEventListener('click', function () {
        film.pause();
        film.loop = false;
        film.setAttribute('controls', '');
        film.setAttribute('aria-label', 'EternalMonitor, the full video');
        (filmStage || filmFrame).classList.add('has-controls');
        setSources(full);
        film.currentTime = 0;
        tryPlay();
        film.focus();
      });
    }
  }

  /* --- GitHub stars, fetched once an hour ---------------------------------- */
  var starEls = document.querySelectorAll('[data-stars]');
  if (starEls.length) {
    var showStars = function (n) {
      var text = n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1).replace(/\.0$/, '') + 'k' : String(n);
      starEls.forEach(function (el) { el.textContent = text; });
    };
    var cached = null;
    try { cached = JSON.parse(sessionStorage.getItem('gh-stars') || 'null'); } catch (e) {}
    if (cached && Date.now() - cached.at < 3600000) {
      showStars(cached.n);
    } else {
      fetch('https://api.github.com/repos/' + REPO, { headers: { Accept: 'application/vnd.github+json' } })
        .then(function (r) { return r.ok ? r.json() : null; })
        .then(function (repo) {
          if (!repo || typeof repo.stargazers_count !== 'number') return;
          showStars(repo.stargazers_count);
          try { sessionStorage.setItem('gh-stars', JSON.stringify({ n: repo.stargazers_count, at: Date.now() })); } catch (e) {}
        })
        .catch(function () {});
    }
  }

  /* --- Known issues: highlight the section in view -------------------------- */
  var tocLinks = Array.prototype.slice.call(document.querySelectorAll('.toc a[href^="#"]'));
  if (tocLinks.length && 'IntersectionObserver' in window) {
    var byId = {};
    tocLinks.forEach(function (a) { byId[a.getAttribute('href').slice(1)] = a; });
    var current = null;
    var sectionObserver = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          if (current) current.classList.remove('active');
          current = byId[entry.target.id];
          if (current) current.classList.add('active');
        }
      });
    }, { rootMargin: '-20% 0px -70% 0px' });
    Object.keys(byId).forEach(function (id) {
      var el = document.getElementById(id);
      if (el) sectionObserver.observe(el);
    });
  }

  /* --- Copy buttons ------------------------------------------------------------ */
  var copyText = function (text, done) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, function () {});
      return;
    }
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try { if (document.execCommand('copy')) done(); } catch (e) {}
    document.body.removeChild(ta);
  };
  document.querySelectorAll('[data-copy]').forEach(function (btn) {
    btn.addEventListener('click', function () {
      var target = document.getElementById(btn.getAttribute('data-copy'));
      if (!target) return;
      var original = btn.textContent;
      copyText(target.textContent.trim(), function () {
        btn.textContent = 'Copied';
        setTimeout(function () { btn.textContent = original; }, 1800);
      });
    });
  });

  /* --- Feedback form: opens the visitor's mail app with the message filled in --- */
  var form = document.getElementById('feedback-form');
  if (form) {
    var stepsField = document.getElementById('fb-steps-field');
    var fallback = document.getElementById('feedback-fallback');
    var preview = document.getElementById('feedback-preview');
    var value = function (id) {
      var el = document.getElementById(id);
      return el ? el.value.trim() : '';
    };
    var kind = function () {
      var checked = form.querySelector('input[name="kind"]:checked');
      return checked ? checked.value : 'Bug';
    };
    var syncKind = function () {
      if (stepsField) stepsField.hidden = kind() !== 'Bug';
      var msg = document.getElementById('fb-message');
      if (msg) {
        msg.placeholder = kind() === 'Bug'
          ? 'What you expected, what happened instead, and any message word for word.'
          : kind() === 'Idea'
            ? 'What would make EternalMonitor more useful for you?'
            : 'Anything at all. How you use it, what you study, what you draw.';
      }
    };
    Array.prototype.forEach.call(form.querySelectorAll('input[name="kind"]'), function (r) {
      r.addEventListener('change', syncKind);
    });
    syncKind();

    var compose = function () {
      var lines = [value('fb-message'), ''];
      if (kind() === 'Bug' && value('fb-steps')) lines.push('Steps to reproduce:', value('fb-steps'), '');
      lines.push(
        'iPad: ' + (value('fb-ipad') || 'not given'),
        'PC: ' + (value('fb-pc') || 'not given'),
        'Connection: ' + (value('fb-connection') || 'not sure'),
        'Release: ' + (form.getAttribute('data-release') || 'not given'),
        'Sent from eternalmonitor.dev/download.html'
      );
      return lines.join('\n');
    };

    form.addEventListener('submit', function (e) {
      e.preventDefault();
      if (!form.reportValidity()) return;
      var tag = { Bug: 'bug', Idea: 'idea', Hello: 'hello' }[kind()] || 'feedback';
      var subject = '[EternalMonitor ' + tag + '] ' + value('fb-message').split('\n')[0].slice(0, 70);
      var body = compose();
      if (preview) preview.textContent = 'To: ' + FEEDBACK_TO + '\nSubject: ' + subject + '\n\n' + body;
      if (fallback) { fallback.hidden = false; fallback.scrollIntoView({ block: 'nearest', behavior: reducedMotion ? 'auto' : 'smooth' }); }
      window.location.href = 'mailto:' + FEEDBACK_TO +
        '?subject=' + encodeURIComponent(subject) + '&body=' + encodeURIComponent(body);
    });
  }

  /* --- Download page: the published release comes from latest.json ------------
     Both apps read the same file for their update checks. The HTML carries
     the same values, so nothing breaks if the fetch fails. */
  var downloadBtn = document.getElementById('download-btn');
  if (downloadBtn) {
    var versionEls = document.querySelectorAll('[data-win-version]');
    var ipadEls = document.querySelectorAll('[data-ipad-version]');
    var metaEl = document.getElementById('release-meta');
    var shaEl = document.getElementById('sha256-value');
    var tfLink = document.getElementById('testflight-link');
    var notesLink = document.getElementById('release-notes-link');

    var formatBytes = function (n) {
      if (n < 1048576) return (n / 1024).toFixed(0) + ' KB';
      return (n / 1048576).toFixed(1) + ' MB';
    };

    fetch('latest.json', { cache: 'no-cache' })
      .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
      .then(function (latest) {
        var win = latest && latest.windows;
        if (!win || !/^\d+\.\d+\.\d+$/.test(win.version)) throw new Error('no windows release');
        var tag = 'v' + win.version;
        var installer = 'https://github.com/' + REPO + '/releases/download/' + tag + '/EternalMonitor-Setup.exe';
        if (win.installer !== installer) throw new Error('unexpected installer');
        downloadBtn.href = installer;
        versionEls.forEach(function (el) { el.textContent = tag; });
        if (shaEl && /^[0-9a-f]{64}$/.test(win.sha256)) shaEl.textContent = win.sha256;
        if (notesLink) notesLink.href = 'https://github.com/' + REPO + '/releases/tag/' + tag;
        var ipad = latest.ipad || {};
        if (ipad.version && ipad.build) {
          ipadEls.forEach(function (el) { el.textContent = 'v' + ipad.version + ', build ' + ipad.build; });
        }
        if (tfLink && /^https:\/\/testflight\.apple\.com\/join\/[A-Za-z0-9]+$/.test(ipad.testflight || '')) {
          tfLink.href = ipad.testflight;
        }
        if (form) {
          form.setAttribute('data-release', 'Windows ' + tag +
            (ipad.version ? ' · iPad v' + ipad.version + (ipad.build ? ', TestFlight build ' + ipad.build : '') : ''));
        }
        return fetch('https://api.github.com/repos/' + REPO + '/releases/tags/' + tag, {
          headers: { Accept: 'application/vnd.github+json' }
        });
      })
      .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
      .then(function (release) {
        var asset = (release.assets || []).filter(function (a) { return a.name === 'EternalMonitor-Setup.exe'; })[0];
        if (asset && metaEl) metaEl.textContent = asset.name + ' · ' + formatBytes(asset.size);
      })
      .catch(function () { /* The HTML already carries the current release. */ });
  }
})();
