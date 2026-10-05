// PostHog setup. Settings come from this script tag's data-* attributes (rendered
// from the POSTHOG_* env vars), so pages need no inline script and the CSP can
// keep inline scripts blocked.
(function () {
	var cfg = document.currentScript.dataset;
	var sampleRate = parseFloat(cfg.sampleRate);
	var replaySampleRate = parseFloat(cfg.replaySampleRate);

	// Shown on the settings page, if it has a status element. This script runs
	// in <head>, so wait for the body before looking for it.
	function showStatus(text) {
		function show() {
			var el = document.getElementById('analytics_status');
			if (el) el.textContent = text;
		}
		if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', show);
		else show();
	}

	// Do Not Track and Global Privacy Control mean no analytics and no recording
	// at all. The server skips its pageviews for the same signals.
	if (navigator.doNotTrack === '1' || window.doNotTrack === '1' || navigator.globalPrivacyControl === true) {
		showStatus('Off: this browser sends Do Not Track or Global Privacy Control.');
		return;
	}

	// Visiting any page once with ?ph_optout=1 opts this browser out (posthog-js
	// remembers it), so operator visits never count. Matches the cluster injector.
	// ?ph_optin=1 undoes it (the settings page links both).
	var optOut = /[?&]ph_optout=1(&|$)/.test(location.search);
	var optIn = !optOut && /[?&]ph_optin=1(&|$)/.test(location.search);

	// Same visitor -> same bucket in [0, 1), so a visitor is always in or out of a sample.
	function bucket(id) {
		var h = 2166136261;
		id = String(id || '');
		for (var i = 0; i < id.length; i++) { h ^= id.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; }
		return h / 4294967295;
	}

	var options = {
		api_host: cfg.host,
		person_profiles: 'always',
		autocapture: false,
		// Don't send a pageview for the visit that opts out.
		capture_pageview: !optOut,
		disable_session_recording: replaySampleRate < 1,
		loaded: function (ph) {
			if (optOut) ph.opt_out_capturing();
			else if (optIn) ph.opt_in_capturing();
			var optedOut = ph.has_opted_out_capturing();
			showStatus(optedOut ? 'You are opted out on this browser.' : 'You are opted in on this browser.');
			if (optedOut) return;
			if (replaySampleRate < 1 && bucket(ph.get_distinct_id()) >= replaySampleRate) return;
			ph.startSessionRecording();
		}
	};

	if (sampleRate < 1) {
		// Session replay ($snapshot) is gated in `loaded` instead; other events keep a fixed share of visitors.
		options.before_send = function (e) {
			if (!e || e.event === '$snapshot') return e;
			if (bucket(e.properties && e.properties.distinct_id) >= sampleRate) return null;
			e.properties.sample_rate = sampleRate;
			return e;
		};
	}

	posthog.init(cfg.key, options);
})();
