<script lang="ts">
  import { kahui, hueOf, initial } from "../state.svelte";

  interface Props {
    onadd: () => void;
  }
  let { onadd }: Props = $props();
</script>

<nav class="rail">
  <div class="list">
    {#each kahui.communities as community (community.id)}
      {@const active = community.id === kahui.communityId}
      <button
        class="pip"
        class:active
        title={community.name}
        onclick={() => kahui.selectCommunity(community.id).catch((e) => kahui.report(e))}
      >
        <!-- The marker on the left is what tells you where you are; the avatar
             only tells communities apart. -->
        <span class="marker" class:on={active}></span>
        <span
          class="avatar"
          style="background: hsl({hueOf(community.id)} 62% 66%)"
        >{initial(community.name)}</span>
      </button>
    {/each}
  </div>

  <button class="pip add" title="Create or join a community" onclick={onadd}>
    <span class="avatar plus">+</span>
  </button>
</nav>

<style>
  .rail {
    width: 68px;
    flex: none;
    background: var(--rail);
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 0.6rem 0;
    gap: 0.35rem;
    overflow-y: auto;
    scrollbar-width: none;
  }
  .rail::-webkit-scrollbar {
    display: none;
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .pip {
    position: relative;
    display: grid;
    place-items: center;
    width: 68px;
    height: 52px;
  }

  .marker {
    position: absolute;
    left: 0;
    width: 3px;
    height: 8px;
    border-radius: 0 3px 3px 0;
    background: var(--text);
    opacity: 0;
    transition: height 0.14s, opacity 0.14s;
  }
  .pip:hover .marker {
    opacity: 0.5;
    height: 18px;
  }
  .marker.on {
    opacity: 1;
    height: 30px;
  }

  .avatar {
    width: 42px;
    height: 42px;
    font-size: 1.02rem;
    transition: border-radius 0.14s, transform 0.14s;
  }
  .pip:hover .avatar {
    border-radius: 32%;
  }
  .pip.active .avatar {
    border-radius: 32%;
  }

  .plus {
    background: var(--raised);
    color: var(--star);
    font-size: 1.4rem;
    font-weight: 400;
    line-height: 1;
  }
  .add:hover .plus {
    background: var(--star);
    color: #1a1405;
  }
</style>
