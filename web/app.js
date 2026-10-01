// clankjob web UI.
//
// Vanilla JavaScript, no build step, no framework. Every piece of text that comes from
// the server (goals, LLM output, emails, answers) is inserted as text, never as HTML.

"use strict";

const TOKEN_KEY = "clankjob.token";
const THEME_KEY = "clankjob.theme";
const THEMES = ["auto", "light", "dark"];

const STATE_LABELS = {
    waiting_for_human: "Needs you",
    running: "Working",
    pending: "Queued",
    sleeping: "Sleeping",
    failed: "Failed",
    completed: "Completed",
    cancelled: "Cancelled",
};
// Rail groups, in order; finished groups start folded.
const GROUP_ORDER = Object.keys(STATE_LABELS);
const FOLDED_BY_DEFAULT = new Set(["completed", "cancelled"]);
const TERMINAL = new Set(["completed", "failed", "cancelled"]);

// Same limit as the server; checked here too so a too-large file fails before uploading.
const MAX_FILE_BYTES = 20 * 1024 * 1024;
const KIND_LABELS = { text: "text", pdf: "PDF", image: "image" };
// Instructions are resent with every LLM turn, so they are kept short (same limits as the server).
const MAX_INSTRUCTIONS = 10;
const MAX_INSTRUCTION_CHARS = 20000;
const MAX_INSTRUCTIONS_CHARS = 50000;

/** Read chosen instruction files as text, checking the limits against what is already there. */
async function readInstructionFiles(files, existing) {
    const added = [];
    for (const file of files) {
        const content = await file.text();
        const total = [...existing, ...added].reduce((sum, item) => sum + item.content.length, content.length);
        if (existing.length + added.length >= MAX_INSTRUCTIONS) {
            toast(`A case holds at most ${MAX_INSTRUCTIONS} instructions.`, "bad");
            break;
        }
        if (!content.trim()) toast(`${file.name} is empty.`, "bad");
        else if (content.length > MAX_INSTRUCTION_CHARS) toast(`${file.name} is longer than ${compact(MAX_INSTRUCTION_CHARS)} characters: add it as a file instead.`, "bad");
        else if (total > MAX_INSTRUCTIONS_CHARS) toast(`Instructions total at most ${compact(MAX_INSTRUCTIONS_CHARS)} characters: add ${file.name} as a file instead.`, "bad");
        else added.push({ name: file.name, content });
    }
    return added;
}

// ---------------------------------------------------------------- DOM helpers

/** Create an element. `props` sets attributes and `on*` listeners; children may be nested arrays. */
function h(tag, props, ...children) {
    const element = document.createElement(tag);
    for (const [key, value] of Object.entries(props || {})) {
        if (value === undefined || value === null || value === false) continue;
        if (key === "class") element.className = value;
        else if (key.startsWith("on")) element.addEventListener(key.slice(2), value);
        else if (value === true) element.setAttribute(key, "");
        else element.setAttribute(key, String(value));
    }
    for (const child of children.flat(Infinity)) {
        if (child === undefined || child === null || child === false || child === "") continue;
        element.append(child instanceof Node ? child : String(child));
    }
    return element;
}

/** A labelled form control with an optional explanation under it. */
function field(label, control, why) {
    return h("label", { class: "field" }, h("span", {}, label), control, why && h("small", { class: "why" }, why));
}

/** A case created with only a title that has not run yet: it waits for the owner's first message. */
function isDraft(item) {
    return !item.goal && item.state === "waiting_for_human" && item.usage.activations === 0;
}

function chip(state) {
    return h("span", { class: `chip ${state}` }, h("i"), STATE_LABELS[state] || state);
}

/** A titled section: small uppercase heading, optional hint, content. */
function section(title, hint, ...content) {
    return h("section", { class: "sec" }, h("div", { class: "sec-h" }, h("h2", {}, title), hint && h("span", { class: "hint" }, hint)), content);
}

function emptyCard(title, text, action) {
    return h("div", { class: "card empty" }, h("h1", {}, title), h("p", {}, text), action);
}

function pretty(value) {
    return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

function kilobytes(bytes) {
    return bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(bytes < 10240 ? 1 : 0)} KB`;
}

// ---------------------------------------------------------------- time and numbers

const relativeFormat = new Intl.RelativeTimeFormat("en", { numeric: "auto" });
const UNITS = [
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
    ["second", 1],
];

function money(usd) {
    if (usd === 0) return "$0";
    if (usd < 0.0001) return "< $0.0001";
    // Cheap models make most cases cost under a cent, so small amounts keep 4 decimals.
    if (usd < 0.01) return `$${usd.toFixed(4)}`;
    return `$${usd.toFixed(usd < 10 ? 2 : 0)}`;
}

/** "Cost ≈ $0.42" for a case's header, explained in its tooltip. */
function costLabel(cost) {
    if (!cost) return "";
    if (cost.usd === null || cost.usd === undefined) {
        return h("span", { title: cost.reason || "" }, "Cost ", h("b", { class: "muted" }, "unknown"));
    }
    const perMillion = (price) => `$${price}/M`;
    const why =
        `Estimate for ${cost.model}: ${cost.input_tokens.toLocaleString()} input tokens × ${perMillion(cost.input_price)} + ` +
        `${cost.output_tokens.toLocaleString()} output tokens × ${perMillion(cost.output_price)}, at today's prices. Cached-token discounts are not counted.`;
    return h("span", { title: why }, "Cost ", h("b", {}, `≈ ${money(cost.usd)}`));
}

function relative(iso) {
    if (!iso) return "never";
    const seconds = Math.round((new Date(iso).getTime() - Date.now()) / 1000);
    if (Math.abs(seconds) < 5) return "just now";
    const [unit, size] = UNITS.find(([, size]) => Math.abs(seconds) >= size) || UNITS[3];
    return relativeFormat.format(Math.round(seconds / size), unit);
}

function timeEl(iso) {
    return h("time", { datetime: iso, title: iso && new Date(iso).toLocaleString() }, relative(iso));
}

function compact(number) {
    return new Intl.NumberFormat("en", { notation: "compact" }).format(number);
}

// ---------------------------------------------------------------- storage, theme, toasts

function readStorage(key) {
    try {
        return localStorage.getItem(key);
    } catch {
        return null;
    }
}

function writeStorage(key, value) {
    try {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
    } catch {
        // Storage can be unavailable (private mode); the UI still works for this tab.
    }
}

let sessionToken = readStorage(TOKEN_KEY);
// Set at startup from /healthz: the server runs with `require_token = false` (testing).
let openAccess = false;

function signedIn() {
    return openAccess || Boolean(sessionToken);
}

/** The Authorization header, when there is a token to send. */
function authHeaders() {
    return sessionToken ? { Authorization: `Bearer ${sessionToken}` } : {};
}

function applyTheme(theme) {
    if (theme === "auto") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", theme);
    const button = document.getElementById("theme-toggle");
    button.textContent = theme[0].toUpperCase() + theme.slice(1);
    button.title = `Theme: ${theme} (click to change)`;
}

function cycleTheme() {
    const current = readStorage(THEME_KEY) || "auto";
    const next = THEMES[(THEMES.indexOf(current) + 1) % THEMES.length];
    writeStorage(THEME_KEY, next);
    applyTheme(next);
}

let lastToast = { message: "", at: 0 };

function toast(message, kind = "") {
    // The same message within a few seconds (e.g. from polling) is shown once.
    if (message === lastToast.message && Date.now() - lastToast.at < 8000) return;
    lastToast = { message, at: Date.now() };
    const element = h("div", { class: `toast ${kind}` }, h("span", {}, message));
    element.append(h("button", { type: "button", "aria-label": "Dismiss", onclick: () => element.remove() }, "×"));
    document.getElementById("toasts").append(element);
    setTimeout(() => element.remove(), kind === "bad" ? 8000 : 4000);
}

// ---------------------------------------------------------------- API

class ApiError extends Error {
    constructor(status, code, message) {
        super(message);
        this.status = status;
        this.code = code;
    }
}

async function api(path, { method = "GET", body } = {}) {
    const headers = authHeaders();
    if (body !== undefined) headers["Content-Type"] = "application/json";
    const response = await fetch(`/api/v1${path}`, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
    });
    const data = await response.json().catch(() => null);
    if (response.status === 401) {
        signOut();
        throw new ApiError(401, "unauthorized", "Please sign in again.");
    }
    if (!response.ok) {
        throw new ApiError(response.status, data?.error?.code, data?.error?.message || response.statusText);
    }
    return data;
}

function report(error) {
    if (error instanceof ApiError && error.status === 401) return;
    toast(error.message || String(error), "bad");
}

/** Run `task` now and then every `ms` while the tab is visible. Returns a stop function. */
function poll(task, ms) {
    let stopped = false;
    let timer = null;
    const tick = async () => {
        if (stopped) return;
        if (!document.hidden) {
            try {
                await task();
            } catch (error) {
                report(error);
            }
        }
        if (!stopped) timer = setTimeout(tick, ms);
    };
    tick();
    return () => {
        stopped = true;
        clearTimeout(timer);
    };
}

// ---------------------------------------------------------------- status bar

function updateCounts(cases) {
    const count = (state) => cases.filter((item) => item.state === state).length;
    const needYou = count("waiting_for_human");
    const parts = [
        [needYou, "need you", needYou > 0],
        [count("running") + count("pending"), "working", false],
        [count("sleeping"), "sleeping", false],
    ];
    document.getElementById("counts").replaceChildren(
        ...parts.flatMap(([n, label, hot], index) => [
            index ? " · " : "",
            h("b", { class: hot ? "hot num" : "num" }, String(n)),
            ` ${label}`,
        ]),
    );
}

function updatePluginAlert(attention) {
    document.getElementById("plugin-alert").hidden = !attention;
}

function updateInboxCount(count) {
    const element = document.getElementById("inbox-count");
    element.hidden = count === 0;
    element.textContent = String(count);
}

let globalPolling = false;

function startGlobalPolling() {
    if (globalPolling) return;
    globalPolling = true;
    poll(async () => {
        if (!signedIn()) return;
        const [{ human_requests: requests }, { cases }, plugins] = await Promise.all([api("/human-requests"), api("/cases?limit=1000"), api("/plugins")]);
        updateInboxCount(requests.length);
        updateCounts(cases);
        updatePluginAlert(plugins.attention);
    }, 10000);
}

function pollHealth() {
    const dot = document.getElementById("dot");
    const text = document.getElementById("pulse-text");
    const sub = document.getElementById("pulse-sub");
    const version = document.getElementById("version");
    poll(async () => {
        const response = await fetch("/healthz").catch(() => null);
        const health = response ? await response.json().catch(() => null) : null;
        const healthy = Boolean(response?.ok);
        dot.className = `dot ${healthy ? "" : "bad"}`;
        text.textContent = healthy ? "engine running" : "engine unreachable";
        sub.textContent = health?.last_scheduler_tick ? `tick ${relative(health.last_scheduler_tick)}` : "";
        if (health?.version) {
            version.textContent = `v${health.version}`;
            version.title = `Version ${health.version}, built from commit ${health.commit}`;
        }
    }, 15000);
}

// ---------------------------------------------------------------- sign in

function signOut() {
    sessionToken = null;
    writeStorage(TOKEN_KEY, null);
    route();
}

function signInView(view) {
    document.body.dataset.view = "page";
    const input = h("input", {
        type: "password",
        name: "token",
        required: true,
        autocomplete: "current-password",
        placeholder: "API token",
        "aria-label": "API token",
    });
    const form = h(
        "form",
        {
            onsubmit: async (event) => {
                event.preventDefault();
                sessionToken = input.value.trim();
                try {
                    await api("/cases?limit=1");
                    writeStorage(TOKEN_KEY, sessionToken);
                    startGlobalPolling();
                    route();
                } catch (error) {
                    toast(error.status === 401 ? "That token was not accepted." : error.message, "bad");
                }
            },
        },
        input,
        h("button", { class: "btn primary", type: "submit" }, "Sign in"),
    );
    view.append(
        h(
            "div",
            { class: "card empty signin" },
            h("span", { class: "brand" }, "clank", h("b", {}, "/"), "job"),
            h("h1", {}, "Sign in"),
            h("p", {}, "Your agents kept working while you were away. Use an API token from clankjob.toml."),
            form,
        ),
    );
    input.focus();
    return () => {};
}

// ---------------------------------------------------------------- new case

/** "OpenAI: GPT-4.1 Mini · $0.40 in · $1.60 out per 1M tokens · 1M context", from what the provider reported. */
function modelLabel(model) {
    const dollars = (price) => `$${price < 10 ? price.toFixed(2) : price.toFixed(0)}`;
    const parts = [];
    if (model.name && model.name !== model.id) parts.push(model.name);
    if (model.input_price !== undefined && model.output_price !== undefined) {
        parts.push(model.input_price === 0 && model.output_price === 0 ? "free" : `${dollars(model.input_price)} in · ${dollars(model.output_price)} out per 1M tokens`);
    }
    if (model.context_length) parts.push(`${compact(model.context_length)} context`);
    return parts.join(" · ");
}

async function newCaseDialog() {
    let profiles = [];
    let llms = { default_llm: "default", llms: [] };
    let channels = [];
    try {
        const [prompts, configured, humanChannels] = await Promise.all([api("/prompts"), api("/llms"), api("/channels")]);
        profiles = prompts.prompts.map((prompt) => prompt.name).filter((name) => name.startsWith("profiles/")).map((name) => name.slice("profiles/".length));
        llms = configured;
        channels = humanChannels.channels;
    } catch (error) {
        report(error);
    }
    const input = (name, props = {}) => h("input", { name, ...props });
    const number = (name, placeholder) => input(name, { type: "number", min: 1, placeholder });

    // Model suggestions follow the selected LLM (including what its provider lists, such
    // as OpenRouter's catalog); any other model id can still be typed.
    const suggestions = h("datalist", { id: "model-suggestions" });
    const modelInput = input("model", { list: "model-suggestions", autocomplete: "off", oninput: () => describeModel() });
    const modelHint = h("small", { class: "why" });
    const llmSelect = h(
        "select",
        { name: "llm", onchange: () => showModelsFor(llmSelect.value) },
        llms.llms.map((llm) => h("option", { value: llm.name, selected: llm.name === llms.default_llm }, llm.name)),
    );
    const selectedLlm = () => llms.llms.find((candidate) => candidate.name === llmSelect.value);
    function describeModel() {
        const llm = selectedLlm();
        const id = modelInput.value.trim() || llm?.model;
        const model = llm?.models.find((candidate) => candidate.id === id);
        const details = model && modelLabel(model);
        modelHint.textContent = details
            ? `${id}: ${details}`
            : `${llm ? `${llm.models.length} models available. ` : ""}Pick one or type any model id; it must support tool calls.`;
    }
    function showModelsFor(name) {
        const llm = llms.llms.find((candidate) => candidate.name === name);
        suggestions.replaceChildren(...(llm?.models || []).map((model) => h("option", { value: model.id, label: modelLabel(model) })));
        modelInput.value = "";
        modelInput.placeholder = llm ? `${llm.model} (default)` : "LLM default";
        describeModel();
    }
    showModelsFor(llms.default_llm);

    // Instructions are read as text in the browser and sent with the case, so the very
    // first run already follows them.
    const instructions = [];
    const instructionList = h("ul", { class: "doc-list" });
    const instructionHint = h("small", { class: "why" });
    const instructionInput = h("input", {
        type: "file",
        multiple: true,
        accept: ".md,.markdown,.txt,text/markdown,text/plain",
        onchange: async () => {
            instructions.push(...(await readInstructionFiles(instructionInput.files, instructions)));
            instructionInput.value = "";
            renderInstructions();
        },
    });
    function renderInstructions() {
        instructionList.replaceChildren(
            ...instructions.map((instruction, index) =>
                h(
                    "li",
                    {},
                    h("span", { class: "mono" }, instruction.name),
                    h("span", { class: "muted num" }, `${compact(instruction.content.length)} chars`),
                    h(
                        "button",
                        {
                            type: "button",
                            class: "btn sm quiet warn",
                            onclick: () => {
                                instructions.splice(index, 1);
                                renderInstructions();
                            },
                        },
                        "Remove",
                    ),
                ),
            ),
        );
        instructionHint.textContent =
            "Markdown or text the agent always follows: tone, constraints, contacts. Kept short, since it is resent every turn; PDFs, images and long material go in Files once the case runs.";
    }
    renderInstructions();

    // Questions always reach the web inbox; these add chat channels such as Discord.
    const channelBoxes = channels.map((channel) => h("input", { type: "checkbox", value: channel.name, checked: channel.default }));
    const channelField =
        channels.length > 0 &&
        h(
            "div",
            { class: "field" },
            h("span", {}, "Also ask me on"),
            h(
                "div",
                { class: "checks" },
                channels.map((channel, index) => h("label", { class: "check" }, channelBoxes[index], h("span", { class: "mono" }, channel.name), h("span", { class: "muted" }, channel.plugin))),
            ),
            h("small", { class: "why" }, "Questions always show up in the inbox here too; the first answer wins."),
        );

    const dialog = h("dialog", { class: "modal", "aria-labelledby": "new-case-title" });
    const close = () => {
        dialog.close();
        dialog.remove();
    };
    const form = h(
        "form",
        { onsubmit: (event) => submit(event).catch(report) },
        h("header", {}, h("h2", { id: "new-case-title" }, "New case")),
        h(
            "div",
            { class: "body" },
            field(
                "Title",
                input("title", { required: true, placeholder: "Electrician quote", autofocus: true }),
                "That is all it takes: the case waits until you send it a message, with files and instructions if you like. Or write the goal under Advanced to start right away.",
            ),
            h(
                "details",
                { class: "more" },
                h("summary", {}, "Advanced"),
                h(
                    "div",
                    { class: "advanced" },
            field(
                "Goal",
                h("textarea", {
                    name: "goal",
                    rows: 5,
                    placeholder: "Get a quote from bob@sparky.ca for a 50A EV charger circuit. Follow up once if he doesn't answer within 2 days.",
                }),
                "Plain language. Say what done looks like and when to give up. With a goal, the agent starts as soon as the case is created.",
            ),
            h(
                "div",
                { class: "two" },
                field("Owner", input("owner", { placeholder: "you" })),
                field("Profile", h("select", { name: "profile" }, h("option", { value: "" }, "Server default"), profiles.map((profile) => h("option", { value: profile }, profile)))),
            ),
            h("div", { class: llms.llms.length > 1 ? "two" : "" }, llms.llms.length > 1 && field("LLM", llmSelect), h("label", { class: "field" }, h("span", {}, "Model"), modelInput, modelHint)),
            h("div", { class: "field" }, h("span", {}, "Instructions"), instructionInput, instructionHint, instructionList),
            channelField,
            (() => {
                const hint = h("small", { class: "why" }, approvalHint("default"));
                const select = approvalSelect("default", { onchange: (event) => (hint.textContent = approvalHint(event.currentTarget.value)) });
                return h("label", { class: "field" }, h("span", {}, "Approvals"), select, hint);
            })(),
            suggestions,
            h(
                "details",
                { class: "more" },
                h("summary", {}, "Budgets"),
                h(
                    "div",
                    { class: "three" },
                    field("Max activations", number("max_activations", "20")),
                    field("Turns per activation", number("max_turns_per_activation", "30")),
                    field("Max tokens", number("max_total_tokens", "2000000")),
                ),
            ),
                ),
            ),
        ),
        h(
            "footer",
            {},
            h("div", { class: "acts" }, h("div", { class: "right" }, h("button", { type: "button", class: "btn quiet", onclick: close }, "Cancel"), h("button", { type: "submit", class: "btn primary" }, "Create case"))),
        ),
    );

    async function submit(event) {
        event.preventDefault();
        const values = Object.fromEntries(new FormData(form));
        const goal = values.goal.trim();
        const body = { title: values.title, llm: llmSelect.value || undefined };
        if (goal) body.goal = goal;
        for (const key of ["owner", "profile", "model"]) if (values[key]) body[key] = values[key].trim();
        if (values.approvals && values.approvals !== "default") body.approvals = values.approvals;
        const budgets = {};
        for (const key of ["max_activations", "max_turns_per_activation", "max_total_tokens"]) if (values[key]) budgets[key] = Number(values[key]);
        if (Object.keys(budgets).length) body.budgets = budgets;
        if (instructions.length) body.instructions = instructions;
        if (channels.length) body.human_channels = channelBoxes.filter((box) => box.checked).map((box) => box.value);
        const created = await api("/cases", { method: "POST", body });
        close();
        toast(goal ? "Case started. The agent is on it." : "Case created. Send it a message to start it.");
        location.hash = `#/cases/${encodeURIComponent(created.id)}`;
    }

    dialog.append(form);
    dialog.addEventListener("cancel", close);
    document.body.append(dialog);
    dialog.showModal();
}

// ---------------------------------------------------------------- the rail

/** The case setting for tool calls that need approval (sending email). */
const APPROVAL_CHOICES = [
    ["default", "Ask, except for trusted contacts", "Emails to trusted contacts go out at once; anything else waits for your OK."],
    ["always", "Always ask", "Every email waits for your OK, even to trusted contacts."],
    ["never", "Never ask", "Emails go out at once, to anyone. Each one is still listed in the timeline."],
];

function approvalSelect(value, props = {}) {
    return h(
        "select",
        { name: "approvals", ...props },
        APPROVAL_CHOICES.map(([choice, label]) => h("option", { value: choice, selected: choice === value }, label)),
    );
}

function approvalHint(value) {
    return (APPROVAL_CHOICES.find(([choice]) => choice === value) || APPROVAL_CHOICES[0])[2];
}

const TRASH_ICON =
    '<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 4h11M6.5 4V2.5h3V4M4 4l.7 9.5h6.6L12 4M6.8 6.5v4.5M9.2 6.5v4.5"/></svg>';

/** A small trash button for a case row; running cases cannot be deleted. */
function trashButton(item, onDelete) {
    if (item.state === "running") return null;
    const button = h("button", {
        class: "trash",
        type: "button",
        title: "Delete this case",
        "aria-label": `Delete case ${item.title}`,
        onclick: (event) => {
            // The button sits in the row's link: don't open the case too.
            event.preventDefault();
            event.stopPropagation();
            onDelete(item);
        },
    });
    button.innerHTML = TRASH_ICON;
    return button;
}

function railRow(item, selectedId, onDelete) {
    const tokens = item.usage.input_tokens + item.usage.output_tokens;
    return h(
        "a",
        { class: item.id === selectedId ? "row sel" : "row", href: `#/cases/${encodeURIComponent(item.id)}`, "aria-current": item.id === selectedId ? "true" : null },
        h("i", { class: `bar ${item.state}`, "aria-hidden": "true" }),
        h("span", { class: "t" }, item.title),
        h("span", { class: "side" }, h("span", { class: "muted num" }, timeEl(item.updated_at)), trashButton(item, onDelete)),
        h(
            "span",
            { class: "m" },
            item.owner && h("span", {}, item.owner),
            item.owner && "·",
            h("span", { class: "num" }, `${item.usage.activations} act.`),
            "·",
            h("span", { class: "num" }, `${compact(tokens)} tok`),
        ),
    );
}

/** The case list, grouped by state. */
function rail() {
    const folded = new Set(FOLDED_BY_DEFAULT);
    let cases = [];
    let selectedId = null;
    const search = h("input", { type: "search", placeholder: "Filter by title, goal, owner…", "aria-label": "Filter cases", autocomplete: "off", oninput: () => render() });
    const list = h("div", { class: "rail-list" }, h("div", { class: "rail-empty" }, "Loading…"));
    const element = h("nav", { class: "rail", "aria-label": "Cases" }, h("div", { class: "rail-tools" }, search), list);

    function render() {
        const query = search.value.trim().toLowerCase();
        const shown = query ? cases.filter((item) => [item.title, item.goal, item.owner || ""].some((text) => text.toLowerCase().includes(query))) : cases;
        if (!cases.length) {
            list.replaceChildren(h("div", { class: "rail-empty" }, "No cases yet. Start one with New case."));
            return;
        }
        if (!shown.length) {
            list.replaceChildren(h("div", { class: "rail-empty" }, "Nothing matches that filter."));
            return;
        }
        list.replaceChildren(
            ...GROUP_ORDER.map((state) => {
                const rows = shown.filter((item) => item.state === state);
                if (!rows.length) return "";
                const isFolded = folded.has(state) && !query;
                return h(
                    "div",
                    { class: isFolded ? "grp folded" : "grp" },
                    h(
                        "h2",
                        { class: "grp-hh" },
                        h(
                            "button",
                            {
                                class: "grp-h",
                                type: "button",
                                "aria-expanded": String(!isFolded),
                                onclick: () => {
                                    if (folded.has(state)) folded.delete(state);
                                    else folded.add(state);
                                    render();
                                },
                            },
                            h("span", { class: "fold", "aria-hidden": "true" }, isFolded ? "▸" : "▾"),
                            STATE_LABELS[state],
                            h("span", { class: "n" }, String(rows.length)),
                        ),
                    ),
                    h("div", { class: "rows" }, rows.map((item) => railRow(item, selectedId, deleteCase))),
                );
            }),
        );
    }

    async function deleteCase(item) {
        const active = !TERMINAL.has(item.state);
        const warning = active ? " It is still active: deleting it stops it." : "";
        if (!window.confirm(`Delete "${item.title}"?${warning} Its timeline, files, notes and questions are removed for good.`)) return;
        try {
            await api(`/cases/${encodeURIComponent(item.id)}`, { method: "DELETE" });
            toast("Case deleted.");
            if (item.id === selectedId) location.hash = "#/cases";
            await refresh();
        } catch (error) {
            report(error);
        }
    }

    async function refresh() {
        cases = (await api("/cases?limit=1000")).cases;
        updateCounts(cases);
        render();
    }

    return {
        element,
        refresh,
        select(id) {
            selectedId = id;
            render();
        },
    };
}

function casesView(view, initialId) {
    const pane = h("main", { class: "detail", id: "detail", tabindex: "-1" });
    const list = rail();
    view.append(h("div", { class: "app" }, list.element, pane));
    let stopDetail = null;
    const stopRail = poll(list.refresh, 4000);

    function select(id) {
        stopDetail?.();
        stopDetail = null;
        list.select(id);
        document.body.dataset.view = id ? "detail" : "list";
        pane.replaceChildren();
        if (id) {
            stopDetail = caseDetail(pane, id, () => list.refresh().catch(report));
            pane.focus({ preventScroll: true });
        } else {
            pane.append(
                h(
                    "div",
                    { class: "detail-in" },
                    emptyCard(
                        "Pick a case",
                        "Choose a case on the left to see what its agent is doing, answer its questions or send it a message. Cases that need you are at the top.",
                        h("div", { class: "acts" }, h("button", { class: "btn primary", type: "button", onclick: () => newCaseDialog() }, "New case")),
                    ),
                ),
            );
        }
    }
    select(initialId);
    return {
        select,
        dispose() {
            stopDetail?.();
            stopRail();
        },
    };
}

// ---------------------------------------------------------------- case detail

function conditionName(kind) {
    return { "core.timer": "timer", "core.human_input": "your answer" }[kind] || kind;
}

function describeWait(wait) {
    const notes = [];
    if (wait.kind === "core.timer") notes.push(["fires ", timeEl(wait.next_check_at)]);
    else if (wait.next_check_at) notes.push(["next check ", timeEl(wait.next_check_at)]);
    if (wait.deadline_at) notes.push(["gives up ", timeEl(wait.deadline_at)]);
    return h("li", {}, h("span", { class: "anchor" }, conditionName(wait.kind)), notes.length > 0 && h("span", { class: "note" }, notes.flatMap((note, index) => [index ? " · " : "", note])));
}

function fact(label, used, limit) {
    const share = limit ? Math.min(1, used / limit) : 0;
    // Width set through the style property: the page's CSP forbids inline style attributes.
    const fill = h("i");
    fill.style.width = `${Math.round(share * 100)}%`;
    return h("div", {}, h("b", {}, limit ? `${compact(used)} / ${compact(limit)}` : String(used)), h("span", {}, label), limit > 0 && h("i", { class: share > 0.8 ? "pip hot" : "pip" }, fill));
}

function answerBox(requestId, onAnswered) {
    const textarea = h("textarea", { name: "text", required: true, rows: 3, placeholder: "Your answer", "aria-label": "Your answer" });
    return h(
        "form",
        {
            class: "box",
            onsubmit: async (event) => {
                event.preventDefault();
                try {
                    await api(`/human-requests/${encodeURIComponent(requestId)}/answer`, { method: "POST", body: { text: textarea.value } });
                    toast("Answer sent. The agent is waking up.");
                } catch (error) {
                    if (error.status === 409) toast("That question was already answered.");
                    else report(error);
                }
                onAnswered();
            },
        },
        textarea,
        h("div", { class: "acts" }, h("div", { class: "right" }, h("button", { class: "btn primary", type: "submit" }, "Send answer"))),
    );
}

/** An approval: what the tool call will do, its arguments (editable), approve or reject. */
function approvalCard(request, caseLink, onAnswered) {
    const args = request.args && typeof request.args === "object" ? request.args : {};
    let editing = false;
    const fields = h("div", { class: "approval-args" });
    const comment = h("input", { name: "comment", placeholder: "Comment for the agent (optional)", "aria-label": "Comment for the agent" });
    const editButton = h("button", { class: "btn quiet", type: "button", onclick: () => toggleEdit() }, "Edit");
    const approveButton = h("button", { class: "btn primary", type: "button", onclick: () => decide("approve") }, "Approve");
    const inputs = new Map();

    function renderFields() {
        inputs.clear();
        fields.replaceChildren(
            ...Object.entries(args).map(([key, value]) => {
                let control;
                if (!editing) {
                    control = h("div", { class: "value" }, typeof value === "string" ? value : pretty(value));
                } else if (typeof value === "boolean") {
                    control = h("input", { type: "checkbox", checked: value });
                    inputs.set(key, () => control.checked);
                } else if (typeof value === "number") {
                    control = h("input", { type: "number", value: String(value) });
                    inputs.set(key, () => Number(control.value));
                } else if (typeof value === "string") {
                    const long = value.length > 80 || value.includes("\n");
                    control = long ? h("textarea", { rows: Math.min(14, value.split("\n").length + 2) }, value) : h("input", { value });
                    inputs.set(key, () => control.value);
                } else {
                    control = h("div", { class: "value mono" }, pretty(value));
                }
                return h("div", { class: "arg" }, h("span", { class: "key" }, key), control);
            }),
        );
    }

    function toggleEdit() {
        editing = !editing;
        editButton.textContent = editing ? "Cancel edit" : "Edit";
        approveButton.textContent = editing ? "Approve edited" : "Approve";
        renderFields();
    }

    async function decide(decision) {
        const body = { decision };
        if (comment.value.trim()) body.comment = comment.value.trim();
        if (decision === "approve" && editing) {
            body.args = { ...args };
            for (const [key, read] of inputs) body.args[key] = read();
        }
        try {
            await api(`/human-requests/${encodeURIComponent(request.id)}/answer`, { method: "POST", body });
            toast(decision === "approve" ? "Approved. It runs as the agent wakes up." : "Rejected. The agent is told why.");
        } catch (error) {
            if (error.status === 409) toast("That approval was already decided.");
            else report(error);
        }
        onAnswered();
    }

    renderFields();
    return h(
        "div",
        { class: "card ask approval" },
        h("div", { class: "who" }, caseLink ? [h("a", { href: `#/cases/${encodeURIComponent(request.case_id)}` }, caseLink), " · "] : "", h("b", {}, "wants your approval"), " · ", timeEl(request.created_at)),
        h("div", { class: "q" }, richMarkdown(request.question)),
        h("div", { class: "hint mono" }, request.tool),
        fields,
        h(
            "div",
            { class: "box" },
            comment,
            h(
                "div",
                { class: "acts" },
                editButton,
                h("div", { class: "right" }, h("button", { class: "btn quiet warn", type: "button", onclick: () => decide("reject") }, "Reject"), approveButton),
            ),
        ),
    );
}

function questionCard(request, caseLink, onAnswered) {
    if (request.kind === "approval") return approvalCard(request, caseLink, onAnswered);
    return h(
        "div",
        { class: "card ask" },
        h("div", { class: "who" }, caseLink ? [h("a", { href: `#/cases/${encodeURIComponent(request.case_id)}` }, caseLink), " · asked "] : "The agent is asking · ", timeEl(request.created_at)),
        h("div", { class: "q" }, richMarkdown(request.question)),
        answerBox(request.id, onAnswered),
    );
}

function argsPreview(call) {
    const args = call.arguments;
    if (call.name === "sleep" && Array.isArray(args?.conditions)) {
        return args.conditions.map((condition) => `${conditionName(condition.kind)}${condition.params?.after ? ` ${condition.params.after}` : ""}`).join(", ");
    }
    const text = typeof args === "string" ? args : JSON.stringify(args);
    return text.length > 70 ? `${text.slice(0, 70)}…` : text;
}

const STATE_SENTENCES = {
    running: "Started working",
    sleeping: "Went to sleep",
    waiting_for_human: "Waiting for you",
    completed: "Case completed",
    failed: "Case failed",
    cancelled: "Case cancelled",
};

/** Renders events into the timeline, attaching tool results to the call that asked for them. */
function timelineRenderer(thread) {
    const calls = new Map();
    const msg = (kind, who, event, ...content) => thread.append(h("div", { class: `msg ${kind}` }, h("div", { class: "who" }, h("b", {}, who), timeEl(event.created_at)), content));
    const txt = (text) => h("div", { class: "txt" }, text);

    function wake(event) {
        const reason = event.payload;
        switch (reason.reason) {
            case "created":
                return msg("bot", "Case created", event);
            case "human_message":
                return msg("human", "You", event, txt(reason.text));
            case "human_answer":
                return msg("human", reason.via && reason.via !== "web" ? `You answered on ${reason.via}` : "You answered", event, h("div", { class: "txt muted" }, reason.question), txt(reason.answer));
            case "approval_decided": {
                const verb = reason.decision === "approve" ? "approved" : "rejected";
                return msg(
                    "human",
                    `You ${verb} ${reason.tool}${reason.via && reason.via !== "web" ? ` on ${reason.via}` : ""}`,
                    event,
                    reason.comment && txt(reason.comment),
                    reason.edited_args && richValue(reason.edited_args, { markdown: false }),
                );
            }
            case "approved_call_finished":
                return msg("", reason.is_error ? `${reason.tool} failed` : `${reason.tool} ran`, event, richValue(reason.result));
            case "condition_fired":
                return msg("", "Woke up", event, txt(`The ${conditionName(reason.kind)} fired.`), reason.details.length > 0 && richValue(reason.details));
            case "timed_out":
                return msg("", "Woke up", event, txt(`Stopped waiting for ${conditionName(reason.kind)}.`));
            case "manual":
                return msg("bot", "Woken up by hand", event);
            case "instructions_changed":
                return msg("human", `You ${reason.change} an instruction`, event, txt(reason.name));
            case "file_added":
                return msg("human", "You added a file", event, txt(`${reason.name} (${KIND_LABELS[reason.kind] || reason.kind})`));
            default:
                return msg("bot", `Woke up (${reason.reason})`, event);
        }
    }

    function agent(event) {
        const message = event.payload;
        const callElements = message.tool_calls.map((call) => {
            const details = h("details", { class: "call" }, h("summary", {}, call.name, h("span", { class: "muted" }, argsPreview(call))), richValue(call.arguments, { markdown: false }));
            calls.set(call.id, details);
            return details;
        });
        msg("agent", "Agent", event, message.text && richMarkdown(message.text), callElements);
    }

    function result(event) {
        const toolResult = event.payload;
        const details = calls.get(toolResult.tool_call_id);
        if (!details) {
            msg("bot", `${toolResult.tool_name} result`, event, richValue(toolResult.content));
            return;
        }
        details.querySelector("summary").append(h("span", { class: toolResult.is_error ? "tag bad" : "tag ok" }, toolResult.is_error ? "error" : "done"));
        details.append(richValue(toolResult.content));
    }

    return (event) => {
        switch (event.kind) {
            case "wake":
                return wake(event);
            case "llm_message":
                return agent(event);
            case "tool_result":
                return result(event);
            case "nudge":
                return msg("bot", "Reminded the agent to use a tool", event);
            case "state_changed":
                // "Queued" transitions are bookkeeping; the wake event already explains them.
                return STATE_SENTENCES[event.payload.to] && msg("bot", STATE_SENTENCES[event.payload.to], event);
            case "error":
                return msg("bad", "Error", event, txt(event.payload.message));
            default:
                return msg("bot", event.kind, event);
        }
    };
}

/** Upload one file to a case as raw bytes; the server detects its type. */
async function uploadFile(caseId, file) {
    const response = await fetch(`/api/v1/cases/${encodeURIComponent(caseId)}/files?name=${encodeURIComponent(file.name)}`, {
        method: "POST",
        headers: { ...authHeaders(), "Content-Type": "application/octet-stream" },
        body: file,
    });
    const data = await response.json().catch(() => null);
    if (response.status === 401) {
        signOut();
        throw new ApiError(401, "unauthorized", "Please sign in again.");
    }
    if (!response.ok) throw new ApiError(response.status, data?.error?.code, data?.error?.message || response.statusText);
    return data;
}

function fileFacts(file) {
    const parts = [KIND_LABELS[file.kind] || file.kind, kilobytes(file.size)];
    if (file.pages) parts.push(`${file.pages} page${file.pages === 1 ? "" : "s"}`);
    if (file.kind === "pdf" && !file.text_chars) parts.push("no text layer");
    return parts.join(" · ");
}

/** A file row: expanding it shows the extracted text, or the image itself. */
function fileRow(caseId, file) {
    const base = `/cases/${encodeURIComponent(caseId)}/files/${encodeURIComponent(file.id)}`;
    const body = h("div", {}, h("pre", { class: "raw" }, "Loading…"));
    let loaded = false;
    async function load() {
        if (file.kind === "image") {
            // An <img> cannot send the bearer token, so the bytes are fetched and shown from a blob URL.
            const response = await fetch(`/api/v1${base}/content`, { headers: authHeaders() });
            if (!response.ok) throw new ApiError(response.status, null, "Could not load this image.");
            const url = URL.createObjectURL(await response.blob());
            body.replaceChildren(h("img", { class: "preview", src: url, alt: file.name }));
        } else {
            const full = await api(base);
            body.replaceChildren(h("pre", { class: "raw" }, full.text || "This PDF has no text layer (probably a scan), so the agent cannot read it."));
        }
    }
    return h(
        "details",
        {
            class: "doc",
            ontoggle: (event) => {
                if (!event.target.open || loaded) return;
                loaded = true;
                load().catch((error) => {
                    loaded = false;
                    body.replaceChildren(h("pre", { class: "raw" }, "Could not load this file."));
                    report(error);
                });
            },
        },
        h("summary", {}, h("span", { class: "mono" }, file.name), h("span", { class: "muted num" }, fileFacts(file))),
        body,
    );
}

/** `onRenamed` lets the case list show a new title without waiting for its next refresh. */
function caseDetail(pane, id, onRenamed) {
    const path = `/cases/${encodeURIComponent(id)}`;
    const head = h("div", { class: "dhead" });
    const questionSlot = h("div");
    const outcomeSlot = h("div");
    const goalSlot = h("div");
    const instructionsList = h("div", { class: "card" });
    const instructionUpload = h("input", {
        type: "file",
        multiple: true,
        hidden: true,
        accept: ".md,.markdown,.txt,text/markdown,text/plain",
        onchange: () => uploadInstructions().catch(report),
    });
    const writeButton = h("button", { class: "btn sm", type: "button", onclick: () => openEditor(null) }, "Write");
    const uploadButton = h("button", { class: "btn sm", type: "button", onclick: () => instructionUpload.click() }, "Upload");
    const instructionsSection = section("Instructions", "always in the agent's context", instructionsList);
    instructionsSection.querySelector(".sec-h").append(h("div", { class: "right" }, writeButton, uploadButton, instructionUpload));
    const filesList = h("div", { class: "card" });
    const fileInput = h("input", { type: "file", multiple: true, hidden: true, onchange: () => addFiles().catch(report) });
    const addButton = h("button", { class: "btn sm", type: "button", onclick: () => fileInput.click() }, "Add files");
    const filesSection = section("Files", "read by the agent only when it needs them", filesList);
    filesSection.querySelector(".sec-h").append(h("div", { class: "right" }, addButton, fileInput));
    const statusSlot = h("div");
    // Redrawn only when the setting changes, so a refresh never closes the menu in use.
    const approvalsSlot = h("div");
    let approvalsKey = null;
    // The same for the model; the LLM choices (GET /llms) are loaded once.
    const modelSlot = h("div");
    let modelKey = null;
    let llmChoices = null;
    // Chat channels the case asks and notifies on, as checkboxes; redrawn only when they
    // change. The loaded channels (GET /channels) are fetched once.
    const channelsSlot = h("div");
    let channelsKey = null;
    let loadedChannels = null;
    // Instructions, files, approvals, notifications and the model, folded under Settings,
    // which starts closed.
    const settingsFacts = h("span", { class: "muted" });
    const settings = h(
        "details",
        { class: "more case-settings" },
        h("summary", {}, "Settings", settingsFacts),
        instructionsSection,
        filesSection,
        approvalsSlot,
        channelsSlot,
        modelSlot,
    );
    const thread = h("div", { class: "card thread" });
    const render = timelineRenderer(thread);
    const hint = h("small", { class: "why" });
    const messageBox = h("textarea", { name: "text", required: true, rows: 3, placeholder: "New information, a change of plan…" });
    const sendButton = h("button", { class: "btn primary", type: "submit" }, "Send message");
    const composer = h(
        "form",
        {
            class: "card box",
            onsubmit: async (event) => {
                event.preventDefault();
                try {
                    await api(`${path}/messages`, { method: "POST", body: { text: messageBox.value } });
                    messageBox.value = "";
                    toast("Message sent.");
                    refresh().catch(report);
                } catch (error) {
                    report(error);
                }
            },
        },
        h("label", { for: "message-box" }, "Message the agent"),
        hint,
        messageBox,
        h("div", { class: "acts" }, h("div", { class: "right" }, sendButton)),
    );
    messageBox.id = "message-box";
    let lastSeq = 0;
    let questionKey = null;
    let filesKey = null;
    let instructionsKey = null;
    let instructionsNow = [];
    let editing = false;
    // While the title is being edited, the periodic refresh leaves the header alone.
    let renaming = false;
    let stop = null;

    const act = (action, message) => async () => {
        try {
            await api(`${path}/${action}`, { method: "POST" });
            toast(message);
            refresh().catch(report);
        } catch (error) {
            report(error);
        }
    };
    const cancel = async () => {
        if (window.confirm("Cancel this case? It stops and cannot be reopened.")) await act("cancel", "Case cancelled.")();
    };

    /** Swap the title for an input: Enter or leaving it saves, Escape cancels. */
    function startRename(heading, item) {
        if (renaming) return;
        renaming = true;
        const input = h("input", { class: "rename", maxlength: 200, "aria-label": "Case title" });
        input.value = item.title;
        let finished = false;
        const finish = async (save) => {
            if (finished) return;
            finished = true;
            const title = input.value.trim();
            try {
                if (save && title && title !== item.title) {
                    await api(path, { method: "PATCH", body: { title } });
                    toast("Case renamed.");
                    onRenamed?.();
                }
            } catch (error) {
                report(error);
            } finally {
                renaming = false;
                refresh().catch(report);
            }
        };
        input.addEventListener("keydown", (event) => {
            if (event.key === "Enter") {
                event.preventDefault();
                finish(true);
            } else if (event.key === "Escape") {
                finish(false);
            }
        });
        input.addEventListener("blur", () => finish(true));
        heading.replaceChildren(input);
        input.focus();
        input.select();
    }

    function renderHead(item, cost) {
        if (renaming) return;
        // A draft starts with the owner's first message, not with a bare wake-up.
        const canWake = (item.state === "sleeping" || item.state === "waiting_for_human") && !isDraft(item);
        head.replaceChildren(
            h(
                "div",
                { class: "line" },
                chip(item.state),
                h("span", { class: "mono muted" }, item.id),
                h(
                    "div",
                    { class: "links" },
                    canWake && h("button", { class: "btn sm", type: "button", onclick: act("wake", "Waking it up.") }, "Wake now"),
                    !TERMINAL.has(item.state) && h("button", { class: "btn sm quiet warn", type: "button", onclick: cancel }, "Cancel case"),
                ),
            ),
            h("h1", { class: "renamable", title: "Double-click to rename", ondblclick: (event) => startRename(event.currentTarget, item) }, item.title),
            h(
                "div",
                { class: "meta" },
                h("span", {}, "Owner ", h("b", {}, item.owner || "not set")),
                h("span", {}, "Profile ", h("b", {}, item.profile || "none")),
                h("span", {}, "Model ", h("b", {}, item.model ? `${item.llm} / ${item.model}` : item.llm)),
                costLabel(cost),
                h("span", {}, "Asks via ", h("b", {}, ["web", ...(item.human_channels || [])].join(", "))),
                h("span", {}, "Created ", h("b", {}, timeEl(item.created_at))),
                h("span", {}, "Updated ", h("b", {}, timeEl(item.updated_at))),
            ),
        );
    }

    function renderQuestions(requests) {
        const key = requests.map((request) => request.id).join(",");
        // Only redrawn when the set changes, so an answer being typed is never wiped.
        if (key === questionKey) return;
        questionKey = key;
        questionSlot.replaceChildren(
            ...requests.map((request) =>
                questionCard(request, null, () => {
                    questionKey = null;
                    refresh().catch(report);
                }),
            ),
        );
    }

    function renderOutcome(item) {
        if (item.state !== "completed" && item.state !== "failed") {
            outcomeSlot.replaceChildren();
            return;
        }
        const ok = item.state === "completed";
        outcomeSlot.replaceChildren(
            h(
                "div",
                { class: ok ? "notice ok" : "notice bad" },
                h("div", { class: "grow" }, h("b", {}, ok ? "Done" : "Failed"), item.outcome ? richMarkdown(item.outcome) : h("p", {}, "No summary given."), item.result !== null && richValue(item.result)),
            ),
        );
    }

    function renderApprovals(item) {
        if (item.approvals === approvalsKey) return;
        approvalsKey = item.approvals;
        const hint = h("small", { class: "why" }, approvalHint(item.approvals));
        const select = approvalSelect(item.approvals, {
            "aria-label": "Approvals for this case",
            disabled: item.state === "cancelled",
            onchange: async (event) => {
                const approvals = event.currentTarget.value;
                hint.textContent = approvalHint(approvals);
                try {
                    await api(path, { method: "PATCH", body: { approvals } });
                    toast("Approvals updated for this case.");
                } catch (error) {
                    report(error);
                }
                approvalsKey = null;
                refresh().catch(report);
            },
        });
        approvalsSlot.replaceChildren(section("Approvals", "when this case's emails wait for you", h("div", { class: "card box approvals" }, select, hint)));
    }

    async function renderModel(item) {
        const key = [item.llm, item.model || "", item.state === "cancelled"].join("\n");
        if (key === modelKey) return;
        modelKey = key;
        if (!llmChoices) {
            try {
                llmChoices = await api("/llms");
            } catch (error) {
                report(error);
                llmChoices = { llms: [] };
            }
        }
        const llms = llmChoices.llms;
        const disabled = item.state === "cancelled";
        const suggestions = h("datalist", { id: "case-model-suggestions" });
        const modelInput = h("input", { name: "model", list: "case-model-suggestions", autocomplete: "off", "aria-label": "Model", disabled, value: item.model || "", oninput: () => describe() });
        const llmSelect = h(
            "select",
            { name: "llm", "aria-label": "LLM", disabled, onchange: () => showModelsFor(true) },
            // The case's LLM stays selectable even if the config no longer lists it.
            (llms.some((llm) => llm.name === item.llm) ? llms : [{ name: item.llm, models: [] }, ...llms]).map((llm) => h("option", { value: llm.name, selected: llm.name === item.llm }, llm.name)),
        );
        const hint = h("small", { class: "why" });
        const selected = () => llms.find((llm) => llm.name === llmSelect.value);
        function describe() {
            const llm = selected();
            const id = modelInput.value.trim() || llm?.model;
            const model = llm?.models.find((candidate) => candidate.id === id);
            hint.textContent = `${model ? `${id}: ${modelLabel(model)}. ` : ""}Used from the agent's next turn. Leave it empty for the LLM's default; the cost estimate prices the whole case at the current model.`;
        }
        function showModelsFor(changed) {
            const llm = selected();
            suggestions.replaceChildren(...(llm?.models || []).map((model) => h("option", { value: model.id, label: modelLabel(model) })));
            // Model ids rarely carry over between providers.
            if (changed) modelInput.value = "";
            modelInput.placeholder = llm ? `${llm.model} (default)` : "LLM default";
            describe();
        }
        showModelsFor(false);
        const form = h(
            "form",
            {
                class: "card box model",
                onsubmit: async (event) => {
                    event.preventDefault();
                    try {
                        await api(path, { method: "PATCH", body: { llm: llmSelect.value, model: modelInput.value.trim() || null } });
                        toast("Model updated for this case.");
                    } catch (error) {
                        report(error);
                    }
                    modelKey = null;
                    refresh().catch(report);
                },
            },
            h("div", { class: "model-row" }, llms.length > 1 && llmSelect, modelInput, suggestions, h("button", { class: "btn sm", type: "submit", disabled }, "Use")),
            hint,
        );
        modelSlot.replaceChildren(section("Model", "which model the agent runs on", form));
    }

    function renderSettings(item, detail) {
        const count = (n, one) => `${n} ${one}${n === 1 ? "" : "s"}`;
        const facts = [count(detail.instructions.length, "instruction"), count(detail.files.length, "file")];
        if (item.approvals !== "default") facts.push(`approvals: ${item.approvals}`);
        if (item.human_channels.length) facts.push(`asks on ${item.human_channels.join(", ")}`);
        // The LLM's default model id, once the Model section has loaded the choices.
        facts.push(item.model || llmChoices?.llms.find((llm) => llm.name === item.llm)?.model || "default model");
        settingsFacts.textContent = facts.join(" · ");
    }

    async function renderChannels(item) {
        const cancelled = item.state === "cancelled";
        const key = [...item.human_channels, cancelled].join("\n");
        if (key === channelsKey) return;
        channelsKey = key;
        if (!loadedChannels) {
            try {
                loadedChannels = (await api("/channels")).channels;
            } catch (error) {
                report(error);
                loadedChannels = [];
            }
        }
        // A channel the case uses stays listed even if its plugin is not loaded right now.
        const names = [...loadedChannels.map((channel) => channel.name), ...item.human_channels.filter((name) => !loadedChannels.some((channel) => channel.name === name))];
        const pluginOf = (name) => loadedChannels.find((channel) => channel.name === name)?.plugin || "not loaded";
        const boxes = names.map((name) =>
            h("input", {
                type: "checkbox",
                value: name,
                checked: item.human_channels.includes(name),
                disabled: cancelled,
                onchange: async () => {
                    const human_channels = boxes.filter((box) => box.checked).map((box) => box.value);
                    try {
                        await api(path, { method: "PATCH", body: { human_channels } });
                        toast(human_channels.length ? `This case now asks on ${human_channels.join(", ")} too.` : "This case now asks in the web inbox only.");
                    } catch (error) {
                        report(error);
                    }
                    channelsKey = null;
                    refresh().catch(report);
                },
            }),
        );
        const body = names.length
            ? [
                  h("div", { class: "checks" }, names.map((name, index) => h("label", { class: "check" }, boxes[index], h("span", { class: "mono" }, name), h("span", { class: "muted" }, pluginOf(name))))),
                  h("small", { class: "why" }, "Questions and the done or failed notification also go to the ticked channels, from the next one on. They always show up in the inbox here; the first answer wins."),
              ]
            : [h("small", { class: "why" }, "No chat channel is loaded, so questions come to the inbox here. Set up the Discord plugin to also get them there.")];
        channelsSlot.replaceChildren(section("Notifications", "where this case asks you and says it is done", h("div", { class: "card box notifications" }, ...body)));
    }

    function renderStatus(detail) {
        const item = detail.case;
        const usage = item.usage;
        // replaceChildren would print `false` as text, unlike h(), so skipped parts are "".
        statusSlot.replaceChildren(
            detail.wait_conditions.length > 0 ? section("Waiting for", "wakes up when any of these happens", h("div", { class: "card" }, h("ul", { class: "ev" }, detail.wait_conditions.map(describeWait)))) : "",
            section(
                "Budget",
                null,
                h(
                    "div",
                    { class: "facts" },
                    fact("activations", usage.activations, item.budgets.max_activations),
                    fact("tokens", usage.input_tokens + usage.output_tokens, item.budgets.max_total_tokens),
                    fact("turn limit per activation", item.budgets.max_turns_per_activation, 0),
                ),
            ),
            section(
                "Notes",
                "facts the agent saved",
                h(
                    "div",
                    { class: "card" },
                    detail.notes.length
                        ? h("ul", { class: "ev" }, detail.notes.map((note) => h("li", {}, h("span", { class: "anchor" }, note.key), h("span", { class: "note" }, note.value))))
                        : h("div", { class: "md muted" }, "Nothing saved yet."),
                ),
            ),
        );
    }

    const instructionsPath = `${path}/instructions`;

    // Redrawn only when an instruction changes, and never while one is being edited.
    function renderInstructions(item, instructions) {
        const cancelled = item.state === "cancelled";
        writeButton.disabled = cancelled;
        uploadButton.disabled = cancelled;
        instructionsNow = instructions;
        const key = `${isDraft(item)}|${instructions.map((instruction) => `${instruction.id}@${instruction.updated_at}`).join(",")}`;
        if (editing || key === instructionsKey) return;
        instructionsKey = key;
        instructionsList.replaceChildren(
            ...(instructions.length
                ? instructions.map((instruction) => instructionRow(instruction, cancelled))
                : [h("div", { class: "md muted" }, isDraft(item) ? "No instructions. Write or upload guidance the agent should always follow; it reads them once your first message starts the case." : "No instructions. Write or upload guidance the agent should always follow; editing one wakes the agent.")]),
        );
    }

    function instructionRow(instruction, cancelled) {
        return h(
            "details",
            { class: "doc" },
            h("summary", {}, h("span", { class: "mono" }, instruction.name), h("span", { class: "muted num" }, `${compact(instruction.content.length)} chars · edited `, timeEl(instruction.updated_at))),
            richString(instruction.content, {}),
            !cancelled &&
                h(
                    "div",
                    { class: "box acts" },
                    h("button", { class: "btn sm", type: "button", onclick: () => openEditor(instruction) }, "Edit"),
                    h("button", { class: "btn sm quiet warn", type: "button", onclick: () => removeInstruction(instruction).catch(report) }, "Remove"),
                ),
        );
    }

    /** An inline editor for a new (`instruction` null) or existing instruction. */
    function openEditor(instruction) {
        editing = true;
        const name = h("input", { name: "name", required: true, value: instruction?.name || "", placeholder: "tone.md" });
        const content = h("textarea", { name: "content", required: true, rows: 10, placeholder: "Always be polite. Never offer more than $1,500." });
        content.value = instruction?.content || "";
        const close = () => {
            editing = false;
            instructionsKey = null;
            refresh().catch(report);
        };
        const form = h(
            "form",
            {
                class: "box",
                onsubmit: async (event) => {
                    event.preventDefault();
                    const body = { name: name.value.trim(), content: content.value };
                    try {
                        if (instruction) await api(`${instructionsPath}/${encodeURIComponent(instruction.id)}`, { method: "PUT", body });
                        else await api(instructionsPath, { method: "POST", body });
                        toast(`${body.name} saved. The agent is waking up to follow it.`);
                        close();
                    } catch (error) {
                        report(error);
                    }
                },
            },
            field("Name", name),
            field("Text", content, `Kept short: at most ${compact(MAX_INSTRUCTION_CHARS)} characters, ${compact(MAX_INSTRUCTIONS_CHARS)} for all instructions together.`),
            h("div", { class: "acts" }, h("div", { class: "right" }, h("button", { class: "btn quiet", type: "button", onclick: close }, "Cancel"), h("button", { class: "btn primary", type: "submit" }, "Save"))),
        );
        instructionsList.replaceChildren(form);
        name.focus();
    }

    async function removeInstruction(instruction) {
        if (!window.confirm(`Remove the instruction ${instruction.name}? The agent will be told.`)) return;
        await api(`${instructionsPath}/${encodeURIComponent(instruction.id)}`, { method: "DELETE" });
        toast(`${instruction.name} removed.`);
        instructionsKey = null;
        refresh().catch(report);
    }

    async function uploadInstructions() {
        const chosen = await readInstructionFiles(instructionUpload.files, instructionsNow);
        instructionUpload.value = "";
        for (const instruction of chosen) {
            try {
                await api(instructionsPath, { method: "POST", body: instruction });
                toast(`${instruction.name} added. The agent is waking up to follow it.`);
            } catch (error) {
                report(error);
            }
        }
        instructionsKey = null;
        refresh().catch(report);
    }

    // Redrawn only when the set of files changes, so an expanded file stays open.
    function renderFiles(item, files) {
        addButton.disabled = item.state === "cancelled";
        const key = `${isDraft(item)}|${files.map((file) => file.id).join(",")}`;
        if (key === filesKey) return;
        filesKey = key;
        filesList.replaceChildren(
            ...(files.length
                ? files.map((file) => fileRow(id, file))
                : [h("div", { class: "md muted" }, isDraft(item) ? "No files yet. Add a quote, a contract, a photo from a contractor: the agent sees them once your first message starts the case." : "No files yet. Add a quote, a contract, a photo from a contractor: the agent wakes up to read it.")]),
        );
    }

    async function addFiles() {
        const chosen = [...fileInput.files];
        fileInput.value = "";
        for (const file of chosen) {
            if (file.size > MAX_FILE_BYTES) {
                toast(`${file.name} is larger than ${kilobytes(MAX_FILE_BYTES)}.`, "bad");
                continue;
            }
            addButton.disabled = true;
            addButton.textContent = `Uploading ${file.name}…`;
            try {
                const stored = await uploadFile(id, file);
                toast(`${stored.name} added (${fileFacts(stored)}). The agent is waking up to read it.`);
            } catch (error) {
                report(error);
            } finally {
                addButton.disabled = false;
                addButton.textContent = "Add files";
            }
        }
        refresh().catch(report);
    }

    function renderComposer(item) {
        const cancelled = item.state === "cancelled";
        messageBox.disabled = cancelled;
        sendButton.disabled = cancelled;
        const draft = isDraft(item);
        messageBox.placeholder = draft ? "What should the agent do? Context, contacts, what done looks like…" : "New information, a change of plan…";
        sendButton.textContent = draft ? "Start" : "Send message";
        hint.textContent = draft
            ? "The case waits for this first message. Add files and instructions first if you like: they do not start it."
            : {
                waiting_for_human: "This answers the agent's open question.",
                completed: "Sending a message reopens the case.",
                failed: "Sending a message reopens the case.",
                cancelled: "Cancelled cases cannot be reopened.",
            }[item.state] || "The agent reads it at its next step, or wakes up for it.";
    }

    async function refresh() {
        let detail;
        try {
            detail = await api(path);
        } catch (error) {
            if (error.status !== 404) throw error;
            stop?.();
            pane.replaceChildren(h("div", { class: "detail-in" }, emptyCard("Case not found", "It may have been created on another server.", h("a", { class: "btn", href: "#/cases" }, "Back to cases"))));
            return;
        }
        const item = detail.case;
        renderHead(item, detail.cost);
        renderQuestions(detail.open_human_requests);
        renderOutcome(item);
        renderComposer(item);
        renderStatus(detail);
        renderApprovals(item);
        renderModel(item).catch(report);
        goalSlot.replaceChildren(item.goal ? section("Goal", null, h("div", { class: "card md" }, item.goal)) : "");
        renderFiles(item, detail.files);
        renderInstructions(item, detail.instructions);
        renderSettings(item, detail);
        renderChannels(item).catch(report);
        const page = await api(`${path}/events?after=${lastSeq}&limit=1000`);
        for (const event of page.events) {
            render(event);
            lastSeq = event.seq;
        }
    }

    pane.append(
        h(
            "div",
            { class: "detail-in" },
            h("a", { class: "btn sm quiet back", href: "#/cases" }, "← Cases"),
            head,
            questionSlot,
            outcomeSlot,
            goalSlot,
            settings,
            statusSlot,
            section("Timeline", "oldest first", thread),
            section("Message", null, composer),
        ),
    );
    stop = poll(refresh, 2500);
    return stop;
}

// ---------------------------------------------------------------- inbox

function pageFrame(view, title, meta, ...actions) {
    document.body.dataset.view = "page";
    const body = h("div");
    view.append(
        h(
            "div",
            { class: "page" },
            h("div", { class: "detail-in" }, h("div", { class: "dhead" }, h("div", { class: "line" }, h("h1", {}, title), h("div", { class: "links" }, actions)), h("div", { class: "meta" }, meta)), body),
        ),
    );
    return body;
}

function inboxView(view) {
    const list = pageFrame(view, "Inbox", "Questions your agents are waiting on. The first answer wins, from here or from any chat channel.");
    const titles = new Map();
    let key = null;

    function caseTitle(caseId) {
        if (!titles.has(caseId)) titles.set(caseId, api(`/cases/${encodeURIComponent(caseId)}`).then((detail) => detail.case.title, () => "Unknown case"));
        return titles.get(caseId);
    }

    async function refresh() {
        const { human_requests: requests } = await api("/human-requests");
        updateInboxCount(requests.length);
        const nextKey = requests.map((request) => request.id).join(",");
        // Re-rendering would wipe answers being typed, so only redraw when the set changes.
        if (nextKey === key) return;
        key = nextKey;
        if (!requests.length) {
            list.replaceChildren(emptyCard("Nothing needs you", "When an agent needs a decision, it asks here and waits patiently."));
            return;
        }
        const cards = await Promise.all(
            requests.map(async (request) =>
                questionCard(request, await caseTitle(request.case_id), () => {
                    key = null;
                    refresh().catch(report);
                }),
            ),
        );
        list.replaceChildren(...cards);
    }

    return poll(refresh, 4000);
}

// ---------------------------------------------------------------- contacts

/** The owner's contacts: the agent looks them up by name; emails to trusted ones need no approval. */
function contactsView(view) {
    const list = pageFrame(
        view,
        "Contacts",
        "People your agents can look up by name (\u201cemail Robin\u201d). Emails to trusted contacts go out without asking you; everyone else still needs your approval.",
    );
    let contacts = [];
    let editingId = null;
    let key = null;
    const listCard = h("div", { class: "contact-list" });

    /** Name, email, phone, note and trusted, filled from `contact` (or empty to add one). */
    function contactForm(contact, onDone) {
        const input = (name, props) => h("input", { name, autocomplete: "off", ...props });
        const form = h(
            "form",
            {
                class: "card box contact-form",
                onsubmit: async (event) => {
                    event.preventDefault();
                    const values = Object.fromEntries(new FormData(form));
                    const body = { name: values.name, trusted: values.trusted === "on" };
                    for (const field of ["email", "phone", "note"]) if (values[field].trim()) body[field] = values[field].trim();
                    try {
                        if (contact) await api(`/contacts/${encodeURIComponent(contact.id)}`, { method: "PUT", body });
                        else {
                            await api("/contacts", { method: "POST", body });
                            form.reset();
                        }
                        toast(contact ? "Contact saved." : "Contact added.");
                        onDone(true);
                    } catch (error) {
                        report(error);
                    }
                },
            },
            h(
                "div",
                { class: "two" },
                field("Name", input("name", { required: true, placeholder: "Robin Tremblay", value: contact?.name })),
                field("Email", input("email", { type: "email", placeholder: "robin@sparky.ca", value: contact?.email })),
            ),
            h(
                "div",
                { class: "two" },
                field("Phone", input("phone", { placeholder: "514 555-0123", value: contact?.phone })),
                field("Note", input("note", { placeholder: "Electrician, quoted the panel", value: contact?.note })),
            ),
            h(
                "label",
                { class: "check" },
                h("input", { type: "checkbox", name: "trusted", checked: Boolean(contact?.trusted) }),
                h("span", {}, "Trusted: emails to them go out without asking me"),
            ),
            h(
                "div",
                { class: "acts" },
                h(
                    "div",
                    { class: "right" },
                    contact && h("button", { type: "button", class: "btn quiet", onclick: () => onDone(false) }, "Cancel"),
                    h("button", { type: "submit", class: "btn primary" }, contact ? "Save" : "Add contact"),
                ),
            ),
        );
        return form;
    }

    async function setTrusted(contact, trusted) {
        const body = { name: contact.name, trusted };
        for (const field of ["email", "phone", "note"]) if (contact[field]) body[field] = contact[field];
        try {
            await api(`/contacts/${encodeURIComponent(contact.id)}`, { method: "PUT", body });
            toast(trusted ? `${contact.name} is trusted: emails to them go out without asking.` : `${contact.name} is no longer trusted.`);
        } catch (error) {
            report(error);
        }
        key = null;
        await refresh();
    }

    async function remove(contact) {
        if (!window.confirm(`Delete ${contact.name} from your contacts?`)) return;
        try {
            await api(`/contacts/${encodeURIComponent(contact.id)}`, { method: "DELETE" });
            toast("Contact deleted.");
        } catch (error) {
            report(error);
        }
        key = null;
        await refresh();
    }

    function row(contact) {
        if (contact.id === editingId) {
            return contactForm(contact, () => {
                editingId = null;
                key = null;
                refresh().catch(report);
            });
        }
        const trustedBox = h("input", {
            type: "checkbox",
            checked: contact.trusted,
            "aria-label": `Trust ${contact.name}`,
            onchange: (event) => setTrusted(contact, event.currentTarget.checked),
        });
        return h(
            "div",
            { class: "contact" },
            h(
                "div",
                { class: "who" },
                h("b", {}, contact.name),
                contact.added_by === "agent" && h("span", { class: "tag new", title: "Added by an agent during a case" }, "added by agent"),
                h("div", { class: "muted" }, [contact.email, contact.phone].filter(Boolean).join(" \u00b7 ") || "no email or phone"),
                contact.note && h("div", { class: "note" }, contact.note),
            ),
            h("label", { class: "check trust" }, trustedBox, h("span", {}, "Trusted")),
            h(
                "div",
                { class: "links" },
                h(
                    "button",
                    {
                        type: "button",
                        class: "btn sm",
                        onclick: () => {
                            editingId = contact.id;
                            key = null;
                            render();
                        },
                    },
                    "Edit",
                ),
                h("button", { type: "button", class: "btn sm quiet warn", onclick: () => remove(contact) }, "Delete"),
            ),
        );
    }

    function render() {
        listCard.replaceChildren(
            ...(contacts.length
                ? contacts.map(row)
                : [emptyCard("No contacts yet", "Add yourself first (name \u201cme\u201d, trusted), so \u201cemail me\u201d works without asking.")]),
        );
    }

    async function refresh() {
        // An edit in progress is never redrawn away.
        if (editingId) return;
        contacts = (await api("/contacts")).contacts;
        const nextKey = contacts.map((contact) => `${contact.id}@${contact.updated_at}`).join(",");
        if (nextKey === key) return;
        key = nextKey;
        render();
    }

    list.append(
        section("Add a contact", null, contactForm(null, () => {
            key = null;
            refresh().catch(report);
        })),
        section("Your contacts", "trusted ones are emailed without asking", listCard),
    );
    return poll(refresh, 15000);
}

// ---------------------------------------------------------------- prompts

/** The owner's own prompt: added to every case's system prompt, saved as user_prompt.md. */
function userPromptSection() {
    const textarea = h("textarea", {
        name: "user_prompt",
        rows: 8,
        placeholder: "Standing instructions for all your cases, e.g.\nMy name is Brad; sign emails as Brad.\nKeep emails short and polite.\nAsk me before agreeing to anything over $500.",
        "aria-label": "Your prompt",
    });
    const counter = h("span", { class: "hint num" });
    const saved = h("span", { class: "hint" });
    const saveButton = h("button", { class: "btn primary", type: "submit" }, "Save");
    let maxChars = 20000;
    let savedContent = "";

    function update() {
        const length = textarea.value.length;
        counter.textContent = `${length.toLocaleString()} / ${maxChars.toLocaleString()} characters`;
        counter.classList.toggle("bad", length > maxChars);
        saveButton.disabled = textarea.value.trim() === savedContent.trim() || length > maxChars;
    }

    function show(prompt) {
        maxChars = prompt.max_chars;
        savedContent = prompt.content;
        textarea.value = prompt.content;
        saved.replaceChildren(...(prompt.updated_at ? ["saved ", timeEl(prompt.updated_at)] : ["not written yet"]));
        update();
    }

    textarea.addEventListener("input", update);
    const form = h(
        "form",
        {
            class: "card box",
            onsubmit: async (event) => {
                event.preventDefault();
                try {
                    show(await api("/user-prompt", { method: "PUT", body: { content: textarea.value } }));
                    toast("Saved. Every case uses it from its next turn.");
                } catch (error) {
                    report(error);
                }
            },
        },
        textarea,
        h("div", { class: "acts" }, counter, saved, h("div", { class: "right" }, saveButton)),
    );
    api("/user-prompt").then(show).catch(report);
    return section("Your prompt", "added to every case's system prompt, from its next turn · data/user_prompt.md", form);
}

function promptsView(view) {
    const reloadButton = h("button", { class: "btn sm", type: "button", onclick: () => reload().catch(report) }, "Reload from disk");
    const body = pageFrame(view, "Prompts", "Every word the agents read. Override any template, or add profiles, in the prompts directory.", reloadButton);
    const errors = h("div");
    const list = h("div", { class: "card prompt-list" });
    const viewer = h("div");
    body.append(userPromptSection(), errors, section("Templates", "built in, or overridden by files in the prompts directory", h("div", { class: "prompt-layout" }, list, viewer)));
    let selected = "system";

    async function show(name) {
        selected = name;
        for (const row of list.querySelectorAll(".row")) row.classList.toggle("sel", row.dataset.name === name);
        const prompt = await api(`/prompts/${name.split("/").map(encodeURIComponent).join("/")}`);
        viewer.replaceChildren(
            h(
                "div",
                { class: "sec-h" },
                h("h2", {}, prompt.name),
                h("span", { class: prompt.source === "file" ? "tag new" : "tag" }, prompt.source === "file" ? "from file" : "built-in"),
                h("span", { class: "hint mono" }, prompt.hash.slice(0, 12)),
            ),
            h("div", { class: "card" }, h("pre", { class: "raw" }, prompt.content)),
        );
    }

    function render(summary) {
        errors.replaceChildren(
            ...summary.errors.map((error) =>
                h("div", { class: "notice bad" }, h("div", { class: "grow" }, h("b", {}, `${error.name} was rejected. `), "The previous version is still in use.", h("pre", { class: "raw" }, error.message))),
            ),
        );
        list.replaceChildren(
            ...summary.prompts.map((prompt) =>
                h(
                    "button",
                    { class: prompt.name === selected ? "row sel" : "row", type: "button", "data-name": prompt.name, onclick: () => show(prompt.name).catch(report) },
                    h("span", { class: "t" }, prompt.name),
                    h("span", { class: "side" }, h("span", { class: prompt.source === "file" ? "tag new" : "tag" }, prompt.source === "file" ? "file" : "built-in")),
                ),
            ),
        );
        return show(summary.prompts.some((prompt) => prompt.name === selected) ? selected : "system");
    }

    async function reload() {
        const summary = await api("/admin/reload", { method: "POST" });
        toast(summary.errors.length ? `Reloaded with ${summary.errors.length} rejected file(s).` : "Prompts reloaded.", summary.errors.length ? "bad" : "");
        await render(summary);
    }

    api("/prompts").then(render).catch(report);
    return () => {};
}

// ---------------------------------------------------------------- plugins

const PLUGIN_STATES = {
    on: ["completed", "On"],
    off: ["", "Off"],
    error: ["failed", "Error"],
};

function pluginStateChip(instance) {
    if (instance.state === "on" && instance.attention) return h("span", { class: "chip waiting_for_human" }, h("i"), "Needs attention");
    const [className, label] = PLUGIN_STATES[instance.state] || ["", instance.state];
    return h("span", { class: `chip ${className}` }, h("i"), label);
}

/** Split "what: fix" as the plugin reports it. */
function issueLine(text) {
    const at = text.indexOf(": ");
    if (at < 0) return h("li", {}, text);
    return h("li", {}, h("b", {}, text.slice(0, at)), h("div", { class: "fix" }, text.slice(at + 2)));
}

function instanceCard(instance, onTest) {
    const report = instance.report || {};
    const activity = instance.activity || {};
    const recentWarning = activity.last_warning_at && Date.now() - new Date(activity.last_warning_at).getTime() < 3600_000;
    const summary = [report.bot && `bot ${report.bot}`, report.channel && `#${report.channel}`].filter(Boolean).join(" in ");
    const findings = instance.findings || [];
    const testButton =
        instance.state === "on" &&
        h(
            "button",
            {
                class: "btn sm",
                type: "button",
                onclick: async (event) => {
                    const button = event.currentTarget;
                    button.disabled = true;
                    button.textContent = "Testing…";
                    try {
                        const tested = await api(`/plugin-instances/${encodeURIComponent(instance.name)}/test`, { method: "POST" });
                        toast(tested.attention ? `${instance.name} still needs attention.` : `${instance.name} looks good.`, tested.attention ? "bad" : "");
                        onTest();
                    } catch (error) {
                        report(error);
                        onTest();
                    }
                },
            },
            "Test now",
        );
    return h(
        "div",
        { class: "card plugin-instance" },
        h(
            "div",
            { class: "line" },
            pluginStateChip(instance),
            h("b", { class: "mono" }, instance.name),
            summary && h("span", { class: "muted" }, summary),
            h("div", { class: "links" }, instance.checked_at && h("span", { class: "hint" }, "checked ", timeEl(instance.checked_at)), testButton),
        ),
        instance.state === "off" && h("p", { class: "muted" }, "Turned off with enabled = false in its config.toml."),
        instance.error && h("div", { class: "notice bad" }, h("div", { class: "grow" }, instance.error)),
        instance.problems.length > 0 && h("div", { class: "notice bad" }, h("div", { class: "grow" }, h("b", {}, "Not working until fixed"), h("ul", { class: "issues" }, instance.problems.map(issueLine)))),
        instance.warnings.length > 0 && h("div", { class: "notice" }, h("div", { class: "grow" }, h("b", {}, "Worth fixing"), h("ul", { class: "issues" }, instance.warnings.map(issueLine)))),
        instance.state === "on" &&
            activity.last_error &&
            h(
                "div",
                { class: activity.last_error_at > (activity.last_ok_at || "") ? "notice bad" : "notice plain" },
                h("div", { class: "grow" }, h("b", {}, "Last error "), timeEl(activity.last_error_at), activity.last_ok_at ? h("span", {}, " · last success ", timeEl(activity.last_ok_at)) : "", h("pre", { class: "raw" }, activity.last_error)),
            ),
        instance.state === "on" && recentWarning && h("div", { class: "notice" }, h("div", { class: "grow" }, h("b", {}, "Reported "), timeEl(activity.last_warning_at), h("pre", { class: "raw" }, activity.last_warning))),
        findings.length > 0 &&
            h(
                "details",
                { class: "more" },
                h("summary", {}, `All checks (${findings.filter((finding) => finding.ok).length}/${findings.length} passed)`),
                h(
                    "ul",
                    { class: "checks-list" },
                    findings.map((finding) =>
                        h("li", { class: finding.ok ? "ok" : finding.required ? "bad" : "warn" }, h("span", { class: "mark" }, finding.ok ? "✓" : finding.required ? "✗" : "!"), h("span", {}, finding.what)),
                    ),
                ),
            ),
    );
}

function pluginsView(view) {
    const reloadButton = h("button", { class: "btn sm", type: "button", onclick: () => reload().catch(report) }, "Reload plugins");
    const body = pageFrame(view, "Plugins", "Loaded from the plugin directories. Edit a plugin's files or its settings and the server reloads it within seconds.", reloadButton);
    const content = h("div");
    body.append(content);
    // Re-rendering while a test runs would reset its button, so polling waits for it.
    let busy = false;

    function render(summary) {
        updatePluginAlert(summary.attention);
        if (!summary.plugin_dirs.length) {
            content.replaceChildren(emptyCard("No plugins directory", "Set plugins_dir in clankjob.toml (e.g. \"./plugin\") to load plugins such as Discord."));
            return;
        }
        if (!summary.plugins.length) {
            content.replaceChildren(emptyCard("No plugins found", `Nothing with a plugin.toml in ${summary.plugin_dirs.join(", ")}.`));
            return;
        }
        content.replaceChildren(
            h("p", { class: "muted mono" }, summary.plugin_dirs.join(" + "), summary.plugin_config_dir ? ` · settings in ${summary.plugin_config_dir}` : ""),
            // replaceChildren would print `false`, so an absent notice is an empty string.
            summary.conflicts.length > 0
                ? h("div", { class: "notice bad" }, h("div", { class: "grow" }, h("b", {}, "Left out because the name is taken"), h("ul", { class: "issues" }, summary.conflicts.map((text) => h("li", {}, text)))))
                : "",
            ...summary.plugins.map((plugin) =>
                section(
                    plugin.name || plugin.id,
                    [plugin.id, plugin.version && `v${plugin.version}`, plugin.runtime, ...plugin.provides.map((what) => what.replace("_", " "))].filter(Boolean).join(" · "),
                    plugin.error && h("div", { class: "notice bad" }, h("div", { class: "grow" }, h("b", {}, "Did not load. "), plugin.error)),
                    plugin.note && h("div", { class: "notice plain" }, h("div", { class: "grow" }, plugin.note)),
                    (plugin.tools.length > 0 || plugin.guides.length > 0 || plugin.conditions.length > 0) &&
                        h(
                            "div",
                            { class: "card plugin-instance" },
                            h("div", { class: "line" }, h("span", { class: "chip completed" }, h("i"), "On"), h("span", { class: "muted" }, "Offered to every case")),
                            plugin.tools.length > 0 && h("dl", { class: "tool-list" }, plugin.tools.flatMap((tool) => [h("dt", { class: "mono" }, tool.name), h("dd", {}, tool.description)])),
                            plugin.conditions.length > 0 &&
                                h(
                                    "dl",
                                    { class: "tool-list" },
                                    plugin.conditions.flatMap((condition) => [h("dt", {}, h("span", { class: "tag" }, "wait for"), " ", h("span", { class: "mono" }, condition.name)), h("dd", {}, condition.description)]),
                                ),
                            plugin.guides.length > 0 &&
                                h(
                                    "dl",
                                    { class: "tool-list" },
                                    plugin.guides.flatMap((guide) => [h("dt", {}, h("span", { class: "tag" }, "guide"), " ", h("span", { class: "mono" }, guide.name)), h("dd", {}, guide.description)]),
                                ),
                        ),
                    plugin.instances.map((instance) =>
                        instanceCard(instance, () => {
                            busy = false;
                            refresh().catch(report);
                        }),
                    ),
                ),
            ),
        );
        for (const button of content.querySelectorAll(".plugin-instance button")) button.addEventListener("click", () => (busy = true));
    }

    async function refresh() {
        if (busy) return;
        render(await api("/plugins"));
    }

    async function reload() {
        reloadButton.disabled = true;
        reloadButton.textContent = "Reloading…";
        try {
            const summary = await api("/plugins/reload", { method: "POST" });
            toast(summary.attention ? "Plugins reloaded; some need attention." : "Plugins reloaded.", summary.attention ? "bad" : "");
            render(summary);
        } finally {
            reloadButton.disabled = false;
            reloadButton.textContent = "Reload plugins";
        }
    }

    return poll(refresh, 5000);
}

// ---------------------------------------------------------------- shell

let current = { section: null, dispose: null, select: null };

function route() {
    const isSignedIn = signedIn();
    document.getElementById("top-actions").hidden = !isSignedIn;
    document.getElementById("counts").hidden = !isSignedIn;
    document.getElementById("sign-out").hidden = openAccess;
    const [section, rawId] = isSignedIn ? (location.hash || "#/cases").slice(2).split("/") : ["signin"];
    const id = rawId ? decodeURIComponent(rawId) : null;
    for (const link of document.querySelectorAll("[data-nav]")) {
        if (link.dataset.nav === section) link.setAttribute("aria-current", "page");
        else link.removeAttribute("aria-current");
    }
    // Moving between cases keeps the rail (and its filter and scroll) and swaps the pane.
    if (section === "cases" && current.section === "cases") {
        current.select(id);
        return;
    }
    current.dispose?.();
    const view = document.getElementById("view");
    view.replaceChildren();
    window.scrollTo(0, 0);
    switch (section) {
        case "signin":
            current = { section, dispose: signInView(view), select: null };
            break;
        case "cases": {
            const cases = casesView(view, id);
            current = { section, dispose: cases.dispose, select: cases.select };
            break;
        }
        case "inbox":
            current = { section, dispose: inboxView(view), select: null };
            break;
        case "prompts":
            current = { section, dispose: promptsView(view), select: null };
            break;
        case "plugins":
            current = { section, dispose: pluginsView(view), select: null };
            break;
        case "contacts":
            current = { section, dispose: contactsView(view), select: null };
            break;
        default:
            current = { section: null, dispose: null, select: null };
            location.hash = "#/cases";
    }
}

applyTheme(readStorage(THEME_KEY) || "auto");
document.getElementById("theme-toggle").addEventListener("click", cycleTheme);
document.getElementById("sign-out").addEventListener("click", signOut);
document.getElementById("new-case").addEventListener("click", () => newCaseDialog());
window.addEventListener("hashchange", route);
pollHealth();
// Ask the server whether it wants a token before showing the sign-in page.
fetch("/healthz")
    .then((response) => response.json())
    .then((health) => {
        openAccess = health?.token_required === false;
    })
    .catch(() => {})
    .finally(() => {
        if (signedIn()) startGlobalPolling();
        route();
    });
