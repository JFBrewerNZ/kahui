<script lang="ts">
  import { kahui, hueOf, initial } from "../state.svelte";
  import { errorText } from "../api";

  // Three things a new device might want, and no others.
  type Pane = "start" | "create" | "join" | "restore";
  let pane = $state<Pane>("start");

  let name = $state(kahui.status?.display_name ?? "");
  let community = $state("");
  let invite = $state("");
  let phrase = $state("");
  let working = $state(false);
  let problem = $state("");

  // A kahui:// link that opened the app lands here: show it, ready to confirm.
  $effect(() => {
    if (!kahui.incomingInvite) return;
    invite = kahui.incomingInvite;
    pane = "join";
    kahui.incomingInvite = "";
  });

  // The generated name is a placeholder, not a choice. Clear it so the field
  // reads as an invitation rather than as something already decided.
  $effect(() => {
    if (name.startsWith("kahui-")) name = "";
  });

  const named = $derived(name.trim().length > 0);

  async function run(what: () => Promise<unknown>) {
    if (working) return;
    working = true;
    problem = "";
    try {
      if (named && name.trim() !== kahui.status?.display_name) {
        await kahui.rename(name.trim());
      }
      await what();
    } catch (err) {
      problem = errorText(err);
    } finally {
      working = false;
    }
  }
</script>

<div class="welcome">
  <div class="card">
    <header>
      <svg viewBox="0 0 64 64" aria-hidden="true">
        <g stroke="#7f93bb" stroke-width="1.9" stroke-linecap="round" opacity="0.5" fill="none">
          <path d="M14 20 L31 11" /><path d="M31 11 L48 21" /><path d="M14 20 L21 37" />
          <path d="M21 37 L40 37" /><path d="M40 37 L48 21" /><path d="M40 37 L53 47" />
          <path d="M21 37 L28 53" /><path d="M28 53 L40 37" />
        </g>
        <g fill="#f4c65a">
          <circle cx="31" cy="11" r="5" /><circle cx="40" cy="37" r="5.4" />
          <circle cx="21" cy="37" r="4.4" /><circle cx="14" cy="20" r="4" />
          <circle cx="28" cy="53" r="4" /><circle cx="48" cy="21" r="3.6" />
          <circle cx="53" cy="47" r="3.4" />
        </g>
      </svg>
      <h1>Kāhui</h1>
      <p class="sub">Communities hosted by their members.</p>
    </header>

    {#if pane === "start"}
      <!-- The identity already exists — a keypair was made the first time this
           ran. There is nothing to sign up for, so the only question is what to
           be called. -->
      <label class="label" for="name">Your name here</label>
      <div class="named">
        <span class="avatar" style="background: hsl({hueOf(kahui.me)} 62% 66%)">
          {initial(name || "?")}
        </span>
        <input
          id="name"
          class="field"
          bind:value={name}
          placeholder="What should people call you?"
          maxlength="64"
          autocomplete="off"
        />
      </div>
      <p class="hint faint">
        Your identity is a key on this device. Nothing to sign up for.
      </p>

      <div class="choices">
        <button class="choice" disabled={!named} onclick={() => (pane = "create")}>
          <b>Start a community</b>
          <span>You will be its first member.</span>
        </button>
        <button class="choice" disabled={!named} onclick={() => (pane = "join")}>
          <b>Join with an invite</b>
          <span>Paste a code or link.</span>
        </button>
      </div>

      {#if !named}
        <p class="nudge faint">Pick a name to carry on.</p>
      {/if}

      <button class="restore-link" onclick={() => (pane = "restore")}>
        Use a key from another device
      </button>
    {:else if pane === "create"}
      <h2>Start a community</h2>
      <p class="hint faint">Starts with a <strong>#general</strong> channel.</p>
      <input
        class="field"
        bind:value={community}
        placeholder="Community name"
        maxlength="64"
        onkeydown={(e) => e.key === "Enter" && community.trim() && run(() => kahui.createCommunity(community))}
      />
      {#if problem}<p class="problem">{problem}</p>{/if}
      <div class="actions">
        <button class="btn" onclick={() => (pane = "start")}>Back</button>
        <button
          class="btn primary"
          disabled={working || !community.trim()}
          onclick={() => run(() => kahui.createCommunity(community))}
        >
          {working ? "Creating…" : "Create"}
        </button>
      </div>
    {:else if pane === "join"}
      <h2>Join a community</h2>

      <textarea
        class="field code mono"
        rows="4"
        bind:value={invite}
        placeholder="kahui1…, kahui://join/…, or a community id"
        spellcheck="false"
      ></textarea>
      {#if problem}<p class="problem">{problem}</p>{/if}
      <div class="actions">
        <button class="btn" onclick={() => (pane = "start")}>Back</button>
        <button
          class="btn primary"
          disabled={working || !invite.trim()}
          onclick={() => run(() => kahui.joinCommunity(invite))}
        >
          {working ? "Joining…" : "Join"}
        </button>
      </div>
    {:else}
      <h2>Use an existing key</h2>
      <p class="hint faint">
        Be the same person on another device. Only works before you have joined anything.
      </p>
      <textarea
        class="field code mono"
        rows="3"
        bind:value={phrase}
        placeholder="kahuikey1…"
        spellcheck="false"
      ></textarea>
      {#if problem}<p class="problem">{problem}</p>{/if}
      <div class="actions">
        <button class="btn" onclick={() => (pane = "start")}>Back</button>
        <button
          class="btn primary"
          disabled={working || !phrase.trim()}
          onclick={() =>
            run(async () => {
              await kahui.restoreIdentity(phrase);
              pane = "start";
            })}
        >
          {working ? "Restoring…" : "Use this key"}
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .welcome {
    height: 100%;
    display: grid;
    place-items: center;
    padding: 2rem 1.5rem;
    overflow-y: auto;
    background-image: radial-gradient(48rem 24rem at 50% -6rem, #17213a 0%, transparent 70%);
    background-repeat: no-repeat;
  }

  .card {
    width: min(31rem, 100%);
  }

  header {
    text-align: center;
    margin-bottom: 1.8rem;
  }
  header svg {
    width: 62px;
    height: 62px;
  }
  h1 {
    margin: 0.6rem 0 0.25rem;
    font-size: 2rem;
    font-weight: 650;
    letter-spacing: -0.02em;
  }
  .sub {
    margin: 0;
    color: var(--dim);
    font-size: 0.95rem;
  }

  h2 {
    margin: 0 0 0.5rem;
    font-size: 1.2rem;
    font-weight: 620;
  }

  .label {
    display: block;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
    margin-bottom: 0.4rem;
  }

  .named {
    display: flex;
    align-items: center;
    gap: 0.65rem;
  }
  .avatar {
    width: 40px;
    height: 40px;
    font-size: 1rem;
  }

  .hint {
    font-size: 0.85rem;
    line-height: 1.6;
    margin: 0.7rem 0 0;
  }

  .choices {
    display: grid;
    gap: 0.6rem;
    margin-top: 1.5rem;
  }
  .choice {
    text-align: left;
    padding: 0.9rem 1rem;
    border-radius: 10px;
    background: var(--raised);
    border: 1px solid var(--line);
    transition: border-color 0.12s, transform 0.12s;
  }
  .choice:hover:not(:disabled) {
    border-color: var(--star);
    transform: translateY(-1px);
  }
  .choice:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .choice b {
    display: block;
    font-weight: 620;
  }
  .choice span {
    display: block;
    color: var(--dim);
    font-size: 0.85rem;
    margin-top: 0.15rem;
  }

  .nudge {
    text-align: center;
    font-size: 0.82rem;
    margin: 0.8rem 0 0;
  }

  .restore-link {
    display: block;
    width: 100%;
    margin-top: 1.4rem;
    padding-top: 1.1rem;
    border-top: 1px solid var(--line);
    color: var(--dim);
    font-size: 0.85rem;
    text-align: center;
  }
  .restore-link:hover {
    color: var(--star);
  }

  .code {
    resize: none;
    font-size: 0.8rem;
    line-height: 1.5;
    word-break: break-all;
    margin-top: 0.9rem;
  }

  .problem {
    margin: 0.8rem 0 0;
    font-size: 0.85rem;
    color: var(--danger);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 1.3rem;
  }
</style>
