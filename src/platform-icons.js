const platformIcons = Object.freeze({
    youtube: 'youtube',
    tiktok: 'tiktok',
    instagram: 'instagram',
    facebook: 'facebook',
    x: 'x-twitter',
    twitter: 'twitter',
    twitch: 'twitch',
    reddit: 'reddit-alien',
    linkedin: 'linkedin',
    vimeo: 'vimeo-v',
});

export const createPlatformIcon = platform => {
    const key = String(platform || '')
        .trim()
        .toLowerCase();
    if (!Object.hasOwn(platformIcons, key)) return null;
    const icon = document.createElement('i');
    icon.className = `fa-brands fa-${platformIcons[key]}`;
    icon.setAttribute('aria-hidden', 'true');
    return icon;
};
