<script lang="ts">
  import { kahui, hueOf, initial } from "../state.svelte";

  // A member is shown as here when this node currently has a connection to
  // them. Anyone else is dimmed rather than marked offline: without a
  // connection we simply do not know, and no node speaks for the whole
  // community.
  let online = $derived(new Set(kahui.online));
</script>

<aside class="pane">
  <div class="section-label">
    Members — {kahui.members.length}
  </div>

  {#each kahui.members as member (member.id)}
    {@const here = member.id === kahui.me || online.has(member.id)}
    <div class="member" class:here>
      <span class="avatar" style="background: hsl({hueOf(member.id)} 62% 66%)">
        {initial(member.display_name)}
      </span>
      <span class="name">
        {member.display_name}
        {#if member.id === kahui.me}<span class="you faint">you</span>{/if}
      </span>
    </div>
  {/each}

  {#if kahui.members.length === 0}
    <p class="empty faint">Nobody here yet.</p>
  {/if}
</aside>

<style>
  .pane {
    width: 200px;
    flex: none;
    background: var(--sidebar);
    padding: 0 0.5rem 1rem;
    overflow-y: auto;
  }

  .member {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.3rem 0.45rem;
    border-radius: 6px;
    opacity: 0.45;
  }
  .member.here {
    opacity: 1;
  }
  .member:hover {
    background: var(--raised);
  }

  .avatar {
    width: 30px;
    height: 30px;
    font-size: 0.8rem;
  }

  .name {
    font-size: 0.88rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .you {
    font-size: 0.72rem;
    margin-left: 0.35rem;
  }

  .empty {
    font-size: 0.82rem;
    padding: 0.4rem 0.45rem;
  }
</style>
