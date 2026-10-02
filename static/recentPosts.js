// "Recent posts" in the feed's right rail, like Reddit's. Threads you open are
// remembered in this browser's localStorage only; nothing is sent anywhere.
(function () {
	var KEY = 'redlib_recent_posts';
	var MAX = 10;

	function load() {
		try {
			var list = JSON.parse(localStorage.getItem(KEY));
			return Array.isArray(list) ? list : [];
		} catch (e) {
			return [];
		}
	}

	function save(list) {
		try {
			localStorage.setItem(KEY, JSON.stringify(list));
		} catch (e) {
			// Storage disabled or full: the panel just stays empty.
		}
	}

	// Only same-site paths, so a stored value can never point elsewhere.
	function local(path) {
		return typeof path === 'string' && path.charAt(0) === '/' && path.charAt(1) !== '/' ? path : '';
	}

	// On a thread page: remember it, newest first, without duplicates.
	var record = document.getElementById('recent_post_data');
	if (record && local(record.dataset.url)) {
		var d = record.dataset;
		var list = load().filter(function (p) { return p.url !== d.url; });
		list.unshift({ url: d.url, title: d.title, community: d.community, thumb: d.thumb, score: d.score, comments: d.comments });
		save(list.slice(0, MAX));
	}

	// On a feed page: render the list. Text goes in via textContent, never as HTML.
	var panel = document.getElementById('recent_posts');
	if (!panel) return;
	var items = load().filter(function (p) { return local(p.url); });
	if (items.length === 0) return;

	var ol = document.getElementById('recent_list');
	items.forEach(function (p) {
		var li = document.createElement('li');

		var text = document.createElement('div');
		text.className = 'recent_text';

		var sub = document.createElement('a');
		sub.className = 'recent_sub';
		sub.href = '/r/' + encodeURIComponent(p.community || '');
		sub.textContent = 'r/' + (p.community || '');

		var title = document.createElement('a');
		title.className = 'recent_title';
		title.href = p.url;
		title.textContent = p.title || p.url;

		var meta = document.createElement('span');
		meta.className = 'recent_meta';
		meta.textContent = (p.score || '0') + ' upvotes · ' + (p.comments || '0') + ' comments';

		text.append(sub, title, meta);
		li.append(text);

		var thumb = local(p.thumb);
		if (thumb) {
			var img = document.createElement('img');
			img.className = 'recent_thumb';
			img.src = thumb;
			img.alt = '';
			img.loading = 'lazy';
			li.append(img);
		}
		ol.append(li);
	});

	panel.hidden = false;
	document.getElementById('recent_clear').addEventListener('click', function () {
		save([]);
		panel.hidden = true;
	});
})();
