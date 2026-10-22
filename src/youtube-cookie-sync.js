export const createYoutubeCookieSync = ({ els, api, schedule = setTimeout, cancel = clearTimeout }) => {
    let current = { state: 'off', cookie_count: 0 };
    let busy = false;
    let loaded = false;
    let revision = 0;
    let timer;
    const status = (message, error = false) => {
        els.youtubeCookieStatus.textContent = message;
        els.youtubeCookieStatus.classList.toggle('oj-status-error', error);
    };
    const render = () => {
        els.youtubeCookieBrowser.disabled = busy;
        els.syncYoutubeCookiesBtn.disabled = busy || !api.available || !loaded;
        els.disconnectYoutubeCookiesBtn.disabled = busy || current.state === 'off';
        els.syncYoutubeCookiesBtn.textContent = ['ready', 'reader'].includes(current.state)
            ? 'Sync Again'
            : 'Enable Cookie Sync';
    };
    const showConnection = () => {
        status(
            current.state === 'ready'
                ? 'YouTube cookies synced from Link Dump for this app session. The yt-dlp reader is available as fallback.'
                : current.state === 'waiting'
                  ? 'Waiting for Link Dump. Open the extension and click Sync YouTube Cookies. Until then, YouTube requests use the yt-dlp browser reader. The extension sync request expires in five minutes.'
                  : current.state === 'reader'
                    ? 'Using the yt-dlp browser reader. Sync from Link Dump to use extension cookies instead.'
                    : 'Cookie sync is off. Enable it to use Link Dump with the yt-dlp reader as fallback.'
        );
        render();
    };
    const poll = () => {
        cancel(timer);
        if (current.state === 'off') return;
        timer = schedule(() => void refresh(), current.state === 'waiting' ? 1000 : 5000);
        timer?.unref?.();
    };
    const refresh = async () => {
        const started = revision;
        try {
            const next = await api.getYoutubeCookieSync();
            if (started !== revision || busy) return;
            current = next;
            if (!loaded && next.fallback_browser) els.youtubeCookieBrowser.value = next.fallback_browser;
            loaded = true;
            showConnection();
        } catch {
            if (started === revision && !busy)
                status('Cookie sync status could not be loaded. Reopen Browser Import to retry.', true);
        }
        poll();
    };
    const change = async enable => {
        if (busy || !api.available || !loaded) return;
        revision++;
        busy = true;
        cancel(timer);
        render();
        try {
            current = enable
                ? await api.beginYoutubeCookieSync({ browser: els.youtubeCookieBrowser.value })
                : await api.disconnectYoutubeCookieSync();
            showConnection();
        } catch {
            status('Cookie sync could not be updated. Please try again.', true);
        } finally {
            busy = false;
            render();
            poll();
        }
    };
    const bindEvents = () => {
        els.syncYoutubeCookiesBtn.addEventListener('click', () => void change(true));
        els.disconnectYoutubeCookiesBtn.addEventListener('click', () => void change(false));
    };
    return Object.freeze({ load: refresh, bindEvents, isLoaded: () => loaded });
};
