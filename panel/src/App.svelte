<template lang="pug">
//- svelte-ignore a11y_no_noninteractive_element_interactions
//- svelte-ignore a11y_interactive_supports_focus
header.navbar(
  class="{isMac ? 'macos' : ''}"
  onmousedown="{handleDrag}"
  ondblclick="{handleDblClick}"
  role="toolbar"
  aria-label="Browser Navigation Bar"
  tabindex="-1"
)
  .nav-group
    button.btn.back(
      title="Back"
      disabled="{!canGoBack}"
      onclick="{goBack}"
      aria-label="Back"
    )
      svg(viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round")
        path(d="m12 19-7-7 7-7m7 7H5")
    button.btn.forward(
      title="Forward"
      disabled="{!canGoForward}"
      onclick="{goForward}"
      aria-label="Forward"
    )
      svg(viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round")
        path(d="M5 12h14m-7-7 7 7-7 7")
    button.btn.reload(
      title="Reload"
      onclick="{reload}"
      aria-label="Reload"
    )
      svg(viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round")
        path(d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8")
        path(d="M21 3v5h-5")
  .url-container
    .url-icon(title="Security Status")
      svg(viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round")
        rect(width="18" height="11" x="3" y="11" rx="2" ry="2")
        path(d="M7 11V7a5 5 0 0 1 10 0v4")
    input(
      type="text"
      bind:value="{inputUrl}"
      placeholder="Search or enter website name"
      onkeydown="{handleKeydown}"
      onfocus="{handleFocus}"
      spellcheck="false"
      autocomplete="off"
    )
  if !isMac
    .window-actions
      button.btn.min(title="Minimize" onclick="{minimize}" aria-label="Minimize")
        svg(viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round")
          path(d="M5 12h14")
      button.btn.max(title="Maximize" onclick="{maximize}" aria-label="Maximize")
        svg(viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round")
          rect(width="18" height="18" x="3" y="3" rx="2")
      button.btn.close(title="Close" onclick="{close}" aria-label="Close")
        svg(viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round")
          path(d="M18 6 6 18M6 6l12 12")
</template>

<script>
  import { onMount } from 'svelte';

  let inputUrl = $state(''),
    canGoBack = $state(false),
    canGoForward = $state(false);

  const isMac = typeof navigator !== 'undefined' && navigator.userAgent.includes('Mac'),
    prompt = (action) => window.prompt(action),
    isInteractive = (e) => e.target.closest('button') || e.target.closest('input');

  const goBack = () => prompt('PREV'),
    goForward = () => prompt('FORWARD'),
    reload = () => prompt('REFRESH'),
    minimize = () => prompt('MINIMIZE'),
    maximize = () => prompt('MAXIMIZE'),
    close = () => window.close(),
    navigate = () => {
      const url = inputUrl?.trim();
      if (url) prompt('NAVIGATE_TO:' + url);
    },
    handleKeydown = (e) => {
      if (e.key === 'Enter') navigate();
    },
    handleFocus = (e) => e.target.select(),
    handleDrag = (e) => {
      if (e.buttons === 1 && !isInteractive(e)) prompt('DRAG_WINDOW');
    },
    handleDblClick = (e) => {
      if (!isInteractive(e)) prompt('DBCLICK_PANEL');
    };

  onMount(() => {
    window.navbar = {
      setNavbarUrl: (url) => {
        inputUrl = url;
      },
      setNavBtnEnabled: (back, forward) => {
        canGoBack = Boolean(back);
        canGoForward = Boolean(forward);
      },
    };
  });
</script>
