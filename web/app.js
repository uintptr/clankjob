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
    const headers = { Authorization: `Bearer ${sessionToken}` };
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
        if (!sessionToken) return;
        const [{ human_requests: requests }, { cases }] = await Promise.all([api("/human-requests"), api("/cases?limit=1000")]);
        updateInboxCount(requests.length);
        updateCounts(cases);
    }, 10000);
}

function pollHealth() {
    const dot = document.getElementById("dot");
    const text = document.getElementById("pulse-text");
    const sub = document.getElementById("pulse-sub");
    poll(async () => {
        const response = await fetch("/healthz").catch(() => null);
        const health = response ? await response.json().catch(() => null) : null;
        const healthy = Boolean(response?.ok);
        dot.className = `dot ${healthy ? "" : "bad"}`;
        text.textContent = healthy ? "engine running" : "engine unreachable";
        sub.textContent = health?.last_scheduler_tick ? `tick ${relative(health.last_scheduler_tick)}` : "";
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
    try {
        const [prompts, configured] = await Promise.all([api("/prompts"), api("/llms")]);
        profiles = prompts.prompts.map((prompt) => prompt.name).filter((name) => name.startsWith("profiles/")).map((name) => name.slice("profiles/".length));
        llms = configured;
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
            field("Title", input("title", { required: true, placeholder: "Electrician quote" })),
            field(
                "Goal",
                h("textarea", {
                    name: "goal",
                    required: true,
                    rows: 5,
                    placeholder: "Get a quote from bob@sparky.ca for a 50A EV charger circuit. Follow up once if he doesn't answer within 2 days.",
                }),
                "Plain language. Say what done looks like and when to give up.",
            ),
            h(
                "div",
                { class: "two" },
                field("Owner", input("owner", { placeholder: "you" })),
                field("Profile", h("select", { name: "profile" }, h("option", { value: "" }, "Server default"), profiles.map((profile) => h("option", { value: profile }, profile)))),
            ),
            h("div", { class: llms.llms.length > 1 ? "two" : "" }, llms.llms.length > 1 && field("LLM", llmSelect), h("label", { class: "field" }, h("span", {}, "Model"), modelInput, modelHint)),
            h("div", { class: "field" }, h("span", {}, "Instructions"), instructionInput, instructionHint, instructionList),
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
        h(
            "footer",
            {},
            h("div", { class: "acts" }, h("div", { class: "right" }, h("button", { type: "button", class: "btn quiet", onclick: close }, "Cancel"), h("button", { type: "submit", class: "btn primary" }, "Start case"))),
        ),
    );

    async function submit(event) {
        event.preventDefault();
        const values = Object.fromEntries(new FormData(form));
        const body = { title: values.title, goal: values.goal, llm: llmSelect.value || undefined };
        for (const key of ["owner", "profile", "model"]) if (values[key]) body[key] = values[key].trim();
        const budgets = {};
        for (const key of ["max_activations", "max_turns_per_activation", "max_total_tokens"]) if (values[key]) budgets[key] = Number(values[key]);
        if (Object.keys(budgets).length) body.budgets = budgets;
        if (instructions.length) body.instructions = instructions;
        const created = await api("/cases", { method: "POST", body });
        close();
        toast("Case started. The agent is on it.");
        location.hash = `#/cases/${encodeURIComponent(created.id)}`;
    }

    dialog.append(form);
    dialog.addEventListener("cancel", close);
    document.body.append(dialog);
    dialog.showModal();
}

// ---------------------------------------------------------------- the rail

function railRow(item, selectedId) {
    const tokens = item.usage.input_tokens + item.usage.output_tokens;
    return h(
        "a",
        { class: item.id === selectedId ? "row sel" : "row", href: `#/cases/${encodeURIComponent(item.id)}`, "aria-current": item.id === selectedId ? "true" : null },
        h("i", { class: `bar ${item.state}`, "aria-hidden": "true" }),
        h("span", { class: "t" }, item.title),
        h("span", { class: "side" }, h("span", { class: "muted num" }, timeEl(item.updated_at))),
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
                    h("div", { class: "rows" }, rows.map((item) => railRow(item, selectedId))),
                );
            }),
        );
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
            stopDetail = caseDetail(pane, id);
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

function questionCard(request, caseLink, onAnswered) {
    return h(
        "div",
        { class: "card ask" },
        h("div", { class: "who" }, caseLink ? [h("a", { href: `#/cases/${encodeURIComponent(request.case_id)}` }, caseLink), " · asked "] : "The agent is asking · ", timeEl(request.created_at)),
        h("div", { class: "q" }, request.question),
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
                return msg("human", "You answered", event, h("div", { class: "txt muted" }, reason.question), txt(reason.answer));
            case "condition_fired":
                return msg("", "Woke up", event, txt(`The ${conditionName(reason.kind)} fired.`), reason.details.length > 0 && h("pre", { class: "raw" }, pretty(reason.details)));
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
            const details = h("details", { class: "call" }, h("summary", {}, call.name, h("span", { class: "muted" }, argsPreview(call))), h("pre", { class: "raw" }, pretty(call.arguments)));
            calls.set(call.id, details);
            return details;
        });
        msg("agent", "Agent", event, message.text && txt(message.text), callElements);
    }

    function result(event) {
        const toolResult = event.payload;
        const details = calls.get(toolResult.tool_call_id);
        if (!details) {
            msg("bot", `${toolResult.tool_name} result`, event, h("pre", { class: "raw" }, pretty(toolResult.content)));
            return;
        }
        details.querySelector("summary").append(h("span", { class: toolResult.is_error ? "tag bad" : "tag ok" }, toolResult.is_error ? "error" : "done"));
        details.append(h("pre", { class: "raw" }, pretty(toolResult.content)));
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
        headers: { Authorization: `Bearer ${sessionToken}`, "Content-Type": "application/octet-stream" },
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
            const response = await fetch(`/api/v1${base}/content`, { headers: { Authorization: `Bearer ${sessionToken}` } });
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

function caseDetail(pane, id) {
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

    function renderHead(item) {
        const canWake = item.state === "sleeping" || item.state === "waiting_for_human";
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
            h("h1", {}, item.title),
            h(
                "div",
                { class: "meta" },
                h("span", {}, "Owner ", h("b", {}, item.owner || "not set")),
                h("span", {}, "Profile ", h("b", {}, item.profile || "none")),
                h("span", {}, "Model ", h("b", {}, item.model ? `${item.llm} / ${item.model}` : item.llm)),
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
                h("div", { class: "grow" }, h("b", {}, ok ? "Done. " : "Failed. "), item.outcome || "No summary given.", item.result !== null && h("pre", { class: "raw" }, pretty(item.result))),
            ),
        );
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
        const key = instructions.map((instruction) => `${instruction.id}@${instruction.updated_at}`).join(",");
        if (editing || key === instructionsKey) return;
        instructionsKey = key;
        instructionsList.replaceChildren(
            ...(instructions.length
                ? instructions.map((instruction) => instructionRow(instruction, cancelled))
                : [h("div", { class: "md muted" }, "No instructions. Write or upload guidance the agent should always follow; editing one wakes the agent.")]),
        );
    }

    function instructionRow(instruction, cancelled) {
        return h(
            "details",
            { class: "doc" },
            h("summary", {}, h("span", { class: "mono" }, instruction.name), h("span", { class: "muted num" }, `${compact(instruction.content.length)} chars · edited `, timeEl(instruction.updated_at))),
            h("pre", { class: "raw" }, instruction.content),
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
        const key = files.map((file) => file.id).join(",");
        if (key === filesKey) return;
        filesKey = key;
        filesList.replaceChildren(
            ...(files.length
                ? files.map((file) => fileRow(id, file))
                : [h("div", { class: "md muted" }, "No files yet. Add a quote, a contract, a photo from a contractor: the agent wakes up to read it.")]),
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
        hint.textContent =
            {
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
        renderHead(item);
        renderQuestions(detail.open_human_requests);
        renderOutcome(item);
        renderComposer(item);
        renderStatus(detail);
        goalSlot.replaceChildren(section("Goal", null, h("div", { class: "card md" }, item.goal)));
        renderFiles(item, detail.files);
        renderInstructions(item, detail.instructions);
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
            instructionsSection,
            filesSection,
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

// ---------------------------------------------------------------- prompts

function promptsView(view) {
    const reloadButton = h("button", { class: "btn sm", type: "button", onclick: () => reload().catch(report) }, "Reload from disk");
    const body = pageFrame(view, "Prompts", "Every word the agents read. Override any template, or add profiles, in the prompts directory.", reloadButton);
    const errors = h("div");
    const list = h("div", { class: "card prompt-list" });
    const viewer = h("div");
    body.append(errors, h("section", { class: "sec" }, h("div", { class: "prompt-layout" }, list, viewer)));
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

// ---------------------------------------------------------------- shell

let current = { section: null, dispose: null, select: null };

function route() {
    const signedIn = Boolean(sessionToken);
    document.getElementById("top-actions").hidden = !signedIn;
    document.getElementById("counts").hidden = !signedIn;
    const [section, rawId] = signedIn ? (location.hash || "#/cases").slice(2).split("/") : ["signin"];
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
if (sessionToken) startGlobalPolling();
route();
