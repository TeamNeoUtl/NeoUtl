(() => {
	alert(
		"NeoUtl及び本サイトは、ＫＥＮくん氏が開発する「AviUtl」及び「AviUtlのお部屋（http://spring-fragrance.mints.ne.jp/aviutl/）」とは一切関係ない、完全に独立した非公式プロジェクトです。AviUtlの公式ソフトウェア、公式サイト、公式派生、公式推奨のいずれでもありません。ＫＥＮくん氏及びAviUtl公式からの承認、許可、関与は一切受けていません。",
	);
})();

const PATTERNS = {
	linux: ["linux"],
	windows: ["win", "msys2", "ucrt", "msvcrt", "msvc", "mingw", "cygwin"],
	apple: ["mac", "apple", "darwin", "xcode"],
};
const ARCH_PATTERNS = {
	x86_64: ["x86_64", "amd64", "x86-64", "universal"],
	arm64: ["aarch64", "arm64", "arm", "universal"],
};
const CACHE_TTL = 60000;
const OBSERVER_MARGIN = "100px";

const findAsset = (assets, platform, arch) => {
	return assets.find((a) => {
		const name = (a.name ?? "").toLowerCase();
		const osMatch = PATTERNS[platform]?.some((s) => name.includes(s));
		if (!osMatch) return false;
		return ARCH_PATTERNS[arch].some((s) => name.includes(s));
	});
};

const formatFileSize = (bytes) => {
	if (typeof bytes !== "number" || !Number.isFinite(bytes)) return "";
	const units = ["B", "KB", "MB", "GB"];
	let value = bytes;
	let unitIndex = 0;
	while (value >= 1024 && unitIndex < units.length - 1) {
		value /= 1024;
		unitIndex += 1;
	}
	return `${value.toFixed(unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
};

const fetchRelease = async (source, repo, tag) => {
	if (source === "codeberg") {
		const res = await fetch(
			`https://codeberg.org/api/v1/repos/${repo}/releases?limit=1`,
		);
		const [latest] = await res.json();
		return {
			tag_name: latest.tag_name,
			name: latest.name,
			created_at: latest.created_at,
			assets: latest.assets,
			html_url: `https://codeberg.org/${repo}/releases/tag/${latest.tag_name}`,
		};
	}
	if (source === "codeberg-tag") {
		const res = await fetch(
			`https://codeberg.org/api/v1/repos/${repo}/releases/tags/${tag}`,
		);
		const latest = await res.json();
		return {
			tag_name: latest.tag_name,
			name: latest.name,
			created_at: latest.created_at,
			assets: latest.assets,
			html_url: `https://codeberg.org/${repo}/releases/tag/${latest.tag_name}`,
		};
	}
	if (source === "github") {
		const res = await fetch(
			`https://api.github.com/repos/${repo}/releases/latest`,
		);
		const latest = await res.json();
		return {
			tag_name: latest.tag_name,
			name: latest.name,
			created_at: latest.published_at,
			assets: latest.assets,
			html_url: latest.html_url,
		};
	}
};

const updateTable = (table, latest) => {
	const suppress = table.dataset.suppressVersion;
	const suppressed = suppress && (latest.tag_name ?? "").includes(suppress);
	const label = suppressed
		? "未リリース"
		: `${latest.name || latest.tag_name} (${new Date(latest.created_at).toLocaleDateString("ja-JP")})`;

	for (const el of table.querySelectorAll(
		".dl-item[data-platform][data-arch]",
	)) {
		const asset =
			!suppressed &&
			findAsset(latest.assets ?? [], el.dataset.platform, el.dataset.arch);
		if (asset) {
			el.href = asset.browser_download_url ?? latest.html_url;
			el.textContent = `${asset.name}（${formatFileSize(asset.size)}）`;
		} else {
			const dash = document.createElement("span");
			dash.textContent = "\u2014";
			el.replaceWith(dash);
		}
	}

	const info = table.querySelector(".dl-info");
	if (info) info.textContent = label;
};

const initTable = (table) => {
	const source = table.dataset.source;
	const repo = table.dataset.repo;
	const tag = table.dataset.tag;
	const cacheKey = `neoutl_release_${source}_${repo}_${tag ?? ""}`;

	const cached = localStorage.getItem(cacheKey);
	if (cached) {
		const { latest, timestamp } = JSON.parse(cached);
		if (Date.now() - timestamp < CACHE_TTL) updateTable(table, latest);
	}

	const observer = new IntersectionObserver(
		async ([entry]) => {
			if (entry.isIntersecting) {
				observer.disconnect();
				try {
					const latest = await fetchRelease(source, repo, tag);
					localStorage.setItem(
						cacheKey,
						JSON.stringify({ latest, timestamp: Date.now() }),
					);
					updateTable(table, latest);
				} catch {
					const info = table.querySelector(".dl-info");
					if (info) info.textContent = "APIエラー";
				}
			}
		},
		{ rootMargin: OBSERVER_MARGIN },
	);
	observer.observe(table);
};

const initReleases = () => {
	document.querySelectorAll(".dl-table[data-source]").forEach(initTable);
};

if ("requestIdleCallback" in window) requestIdleCallback(initReleases);
else {
	setTimeout(initReleases, 1);
}

document.addEventListener("click", ({ target }) => {
	if (target.tagName === "CODE") {
		navigator.clipboard.writeText(target.textContent).then(() => {
			target.classList.add("is-copied");
			setTimeout(() => target.classList.remove("is-copied"), 1500);
		});
	}
});

document.querySelectorAll("table").forEach((table) => {
	const wrapper = document.createElement("div");
	wrapper.className = "table-scroll";
	table.parentNode.insertBefore(wrapper, table);
	wrapper.appendChild(table);
});

// GAS アクセスカウンター
const GAS_APP_URL =
	"https://script.google.com/macros/s/AKfycbw8vCfU7uXxWkjeZ1h_yqvOitYqocgENVS1YFa8JVa347a1TpCsqWhs4m05Kpy7p-vCSw/exec";

const getJstDateParts = (offsetDays) => {
	const now = new Date();
	const jst = new Date(now.getTime() + 9 * 3600000 + offsetDays * 86400000);
	return {
		year: jst.getUTCFullYear(),
		month: jst.getUTCMonth() + 1,
		day: jst.getUTCDate(),
	};
};

const renderCounter = async () => {
	const el = document.getElementById("neoutl-counter");
	const todayParts = getJstDateParts(0);
	const yesterdayParts = getJstDateParts(-1);
	const todayKey = `neoutl-counter-${todayParts.year}-${todayParts.month}-${todayParts.day}`;
	const yesterdayKey = `neoutl-counter-${yesterdayParts.year}-${yesterdayParts.month}-${yesterdayParts.day}`;

	try {
		const res = await fetch(
			`${GAS_APP_URL}?today=${todayKey}&yesterday=${yesterdayKey}`,
		);
		const data = await res.json();
		if (data.error) {
			el.innerHTML = `今日: <b>?</b> / 昨日: <b>?</b> / 累計: <b>?</b>`;
			return;
		}
		el.innerHTML = `今日: <b>${data.today}</b> / 昨日: <b>${data.yesterday}</b> / 累計: <b>${data.total}</b>`;
	} catch {
		el.innerHTML = `今日: <b>?</b> / 昨日: <b>?</b> / 累計: <b>?</b>`;
	}
};

renderCounter();

// 最近のコミット取得
const CACHE_KEY_COMMITS = "neoutl_commits_main";

const renderCommits = (commits) => {
	const tbody = document.getElementById("commit-tbody");
	if (!tbody) return;

	const loadingRow = document.getElementById("commit-loading-row");
	if (loadingRow) loadingRow.remove();

	if (!commits || commits.length === 0) {
		const tr = document.createElement("tr");
		tr.innerHTML =
			'<td colspan="2" class="text-small">コミット履歴がありません。</td>';
		tbody.insertBefore(tr, tbody.lastElementChild);
		return;
	}

	const lastRow = tbody.lastElementChild;

	commits.forEach((item) => {
		const message = item.commit.message.split("\n")[0];
		const date = new Date(item.commit.author.date).toLocaleDateString("ja-JP", {
			year: "numeric",
			month: "2-digit",
			day: "2-digit",
		});
		const url = `https://codeberg.org/TeamNeoUtl/NeoUtl/commit/${item.sha}`;

		const tr = document.createElement("tr");
		tr.style.cursor = "pointer";
		tr.addEventListener("click", () => {
			window.open(url, "_blank");
		});
		tr.innerHTML = `
        <td class="text-center text-small">${date}</td>
        <td><a href="${url}" target="_blank" rel="noopener">${message}</a></td>
      `;
		tbody.insertBefore(tr, lastRow);
	});
};

const fetchLatestCommits = async () => {
	try {
		const res = await fetch(
			"https://codeberg.org/api/v1/repos/TeamNeoUtl/NeoUtl/commits?sha=main&limit=3",
		);
		if (!res.ok) throw new Error(`HTTP ${res.status}`);
		const commits = await res.json();
		localStorage.setItem(
			CACHE_KEY_COMMITS,
			JSON.stringify({ commits, timestamp: Date.now() }),
		);
		renderCommits(commits);
	} catch {
		const loadingRow = document.getElementById("commit-loading-row");
		if (loadingRow) {
			loadingRow.innerHTML =
				'<td colspan="2" class="text-small">コミット履歴の取得に失敗しました。</td>';
		}
	}
};

// Lazy-load commits with IntersectionObserver (cached)
const initCommits = () => {
	const table = document.getElementById("commit-table");
	if (!table) return;

	const cached = localStorage.getItem(CACHE_KEY_COMMITS);
	if (cached) {
		const { commits, timestamp } = JSON.parse(cached);
		if (Date.now() - timestamp < CACHE_TTL) {
			renderCommits(commits);
			return;
		}
	}

	const observer = new IntersectionObserver(
		([entry]) => {
			if (entry.isIntersecting) {
				observer.disconnect();
				fetchLatestCommits();
			}
		},
		{ rootMargin: "100px" },
	);
	observer.observe(table);
};

if ("requestIdleCallback" in window) requestIdleCallback(initCommits);
else setTimeout(initCommits, 1);
