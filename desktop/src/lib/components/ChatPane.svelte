<script lang="ts">
  import { kahui, hueOf, initial, clock, dayLabel, sameDay } from "../state.svelte";
  import type { Message } from "../api";

  /** Consecutive messages from one author within this long are grouped. */
  const GROUP_WINDOW_MS = 5 * 60 * 1000;
  /** How close to the bottom counts as "following along". */
  const STICK_PX = 120;

  let scroller = $state<HTMLDivElement | null>(null);
  let draft = $state("");
  let sending = $state(false);
  let pinned = $state(true);

  function atBottom() {
    if (!scroller) return true;
    return scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < STICK_PX;
  }

  // Follow new messages, but only if the reader had not scrolled away to read
  // something older. Yanking them back would be worse than missing a line.
  $effect(() => {
    kahui.messages.length;
    if (!scroller || !pinned) return;
    queueMicrotask(() => scroller?.scrollTo({ top: scroller.scrollHeight }));
  });

  // Jump straight to the newest message when a channel is opened.
  $effect(() => {
    kahui.channelId;
    pinned = true;
  });

  async function submit(event: Event) {
    event.preventDefault();
    const body = draft.trim();
    if (!body || sending) return;
    sending = true;
    // Clear immediately: the message is already committed locally by the time
    // post() returns, and a field that lingers feels broken.
    draft = "";
    pinned = true;
    try {
      await kahui.send(body);
    } catch (err) {
      draft = body;
      kahui.report(err);
    } finally {
      sending = false;
    }
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.shiftKey) {
      submit(event);
    }
  }

  function startsGroup(message: Message, previous: Message | undefined): boolean {
    if (!previous) return true;
    if (previous.author !== message.author) return true;
    return message.timestamp_ms - previous.timestamp_ms > GROUP_WINDOW_MS;
  }
</script>

<section class="chat">
  <header>
    {#if kahui.channel}
      <span class="hash">#</span>
      <span class="title">{kahui.channel.name}</span>
      {#if kahui.channel.topic}
        <span class="divider"></span>
        <span class="topic muted">{kahui.channel.topic}</span>
      {/if}
    {:else}
      <span class="title muted">No channel selected</span>
    {/if}
  </header>

  <div
    class="scroll"
    bind:this={scroller}
    onscroll={() => (pinned = atBottom())}
  >
   <div class="feed">
    {#if kahui.channel && kahui.messages.length === 0}
      <div class="opening">
        <h3>#{kahui.channel.name}</h3>
        <p class="muted">The start of the channel.</p>
      </div>
    {/if}

    {#each kahui.messages as message, i (message.id)}
      {@const previous = kahui.messages[i - 1]}
      {#if !previous || !sameDay(previous.timestamp_ms, message.timestamp_ms)}
        <div class="day"><span>{dayLabel(message.timestamp_ms)}</span></div>
      {/if}

      {#if startsGroup(message, previous)}
        <div class="message lead">
          <span class="avatar" style="background: hsl({hueOf(message.author)} 62% 66%)">
            {initial(message.author_name)}
          </span>
          <div class="body">
            <div class="byline">
              <span class="author" class:mine={message.author === kahui.me}>
                {message.author_name}
              </span>
              <time title={new Date(message.timestamp_ms).toLocaleString()}>
                {clock(message.timestamp_ms)}
              </time>
            </div>
            <p>{message.body}</p>
          </div>
        </div>
      {:else}
        <div class="message cont">
          <time class="gutter" title={new Date(message.timestamp_ms).toLocaleString()}>
            {clock(message.timestamp_ms)}
          </time>
          <p>{message.body}</p>
        </div>
      {/if}
    {/each}
   </div>
  </div>

  {#if !pinned && kahui.messages.length > 0}
    <button class="jump" onclick={() => { pinned = true; scroller?.scrollTo({ top: scroller.scrollHeight }); }}>
      Jump to the latest ↓
    </button>
  {/if}

  <form onsubmit={submit}>
    <textarea
      class="composer"
      rows="1"
      bind:value={draft}
      {onkeydown}
      disabled={!kahui.channel}
      placeholder={kahui.channel ? `Message #${kahui.channel.name}` : "Choose a channel first"}
    ></textarea>
  </form>
</section>

<style>
  .chat {
    flex: 1;
    min-width: 0;
    background: var(--chat);
    display: flex;
    flex-direction: column;
    position: relative;
  }

  header {
    height: 48px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0 1rem;
    border-bottom: 1px solid var(--rail);
    box-shadow: 0 1px 0 rgba(0, 0, 0, 0.2);
  }
  .hash {
    color: var(--faint);
    font-weight: 600;
    font-size: 1.05rem;
  }
  .title {
    font-weight: 620;
  }
  .divider {
    width: 1px;
    height: 18px;
    background: var(--line);
    margin: 0 0.4rem;
  }
  .topic {
    font-size: 0.85rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    /* The transcript is the one place where selecting text is the point. */
    user-select: text;
    cursor: auto;
  }

  /* Pushes a short transcript down to the composer, where the next message will
     appear, instead of leaving it stranded at the top of an empty pane. */
  .feed {
    margin-top: auto;
    padding: 1rem 0 1.2rem;
  }

  .opening {
    padding: 1.5rem 1.2rem 0.5rem;
  }
  .opening h3 {
    margin: 0 0 0.4rem;
    font-size: 1.5rem;
  }
  .opening p {
    margin: 0;
    max-width: 34rem;
    font-size: 0.9rem;
  }

  .day {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin: 1.1rem 1.2rem 0.6rem;
    color: var(--faint);
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .day::before,
  .day::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--line);
  }

  .message {
    padding: 0.08rem 1.2rem;
  }
  .message:hover {
    background: rgba(255, 255, 255, 0.018);
  }
  .message.lead {
    display: flex;
    gap: 0.7rem;
    margin-top: 0.7rem;
  }

  .avatar {
    width: 38px;
    height: 38px;
    font-size: 0.95rem;
    margin-top: 0.1rem;
  }

  .body {
    min-width: 0;
    flex: 1;
  }
  .byline {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
  }
  .author {
    font-weight: 620;
  }
  .author.mine {
    color: var(--star);
  }
  time {
    font-size: 0.72rem;
    color: var(--faint);
  }

  p {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  /* Continued messages line up under the first, with the time appearing on
     hover in the gutter where the avatar would be. */
  .message.cont {
    display: flex;
    gap: 0.7rem;
  }
  .gutter {
    width: 38px;
    flex: none;
    text-align: right;
    opacity: 0;
    font-size: 0.68rem;
    line-height: 1.55;
  }
  .message.cont:hover .gutter {
    opacity: 1;
  }

  .jump {
    position: absolute;
    right: 1.2rem;
    bottom: 4.6rem;
    padding: 0.4rem 0.75rem;
    font-size: 0.8rem;
    border-radius: 999px;
    background: var(--raised);
    border: 1px solid var(--line);
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.4);
  }
  .jump:hover {
    background: var(--hover);
  }

  form {
    flex: none;
    padding: 0 1.2rem 1.1rem;
  }
  .composer {
    width: 100%;
    max-height: 9rem;
    padding: 0.7rem 0.9rem;
    background: var(--raised);
    border: 1px solid transparent;
    border-radius: 10px;
    resize: none;
    field-sizing: content;
  }
  .composer:focus {
    border-color: var(--line);
  }
  .composer::placeholder {
    color: var(--faint);
  }
  .composer:disabled {
    opacity: 0.6;
  }
</style>
