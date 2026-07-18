// Applies the selected theme instantly on the settings page and persists it
// in the theme cookie, so no explicit save/reload is needed. The form's
// normal save flow still works without JavaScript.
(function () {
	const select = document.getElementById("theme");
	if (!select) return;

	const themes = Array.from(select.options)
		.map(function (o) { return o.value; })
		.filter(function (v) { return v && v !== "system"; });

	select.addEventListener("change", function () {
		const chosen = select.value;
		themes.forEach(function (t) { document.body.classList.remove(t); });
		if (chosen !== "system") {
			document.body.classList.add(chosen);
		}
		fetch("/settings/update/?theme=" + encodeURIComponent(chosen) + "&redirect=settings", {
			credentials: "same-origin",
		}).catch(function () {});
	});
})();
