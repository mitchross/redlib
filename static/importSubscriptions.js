// Shrink a pasted mine.json to just the subreddit names before it is uploaded.
// A page of 100 subreddits is megabytes of JSON, almost all of it descriptions
// and sidebar HTML that redlib throws away. CSV and plain lists are sent as is,
// and without JavaScript the server parses the full JSON itself.
(function () {
	var form = document.getElementById('import_subscriptions');
	if (!form) return;

	form.addEventListener('submit', function () {
		var field = form.elements.import_data;
		var json;
		try {
			json = JSON.parse(field.value);
		} catch (e) {
			return;
		}

		// Same rule as the server: every "display_name" anywhere in the listing.
		var names = [];
		(function collect(value) {
			if (Array.isArray(value)) {
				value.forEach(collect);
			} else if (value && typeof value === 'object') {
				if (typeof value.display_name === 'string') names.push(value.display_name);
				Object.keys(value).forEach(function (key) { collect(value[key]); });
			}
		})(json);
		if (names.length === 0) return;

		field.value = names.join('\n');
		// Keep Reddit's next-page cursor so the settings page can link the next page.
		var after = json && json.data && json.data.after;
		if (typeof after === 'string') form.elements.import_after.value = after;
	});
})();
