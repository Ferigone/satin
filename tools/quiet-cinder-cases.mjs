export const DESIGN_SHA256 = 'e642b9e492e9dfc738fefce3bfd5a09124fca6e9515deeeecc373022d41373bd';

export const viewports = {
  desktop: [1280, 800],
  narrow: [640, 480],
};

export const states = [
  'idle', 'tab-hover', 'quiet', 'menu', 'menu-hover', 'menu-dismiss',
  'address', 'settings', 'bookmarks', 'downloads',
];

// Rectangles are [left, top, width, height] in client pixels at 100%. Bounds
// come from the reference CSS, not candidate screenshots or auto-alignment.
const wide = {
  caption: [0, 0, 98, 44],
  'window-controls': [1152, 0, 128, 44],
  tabs: [104, 3, 660, 39],
  'inactive-tab': [299, 5, 195, 34],
  navigation: [7, 45, 159, 44],
  'address-field': [165, 47, 350, 40],
  'page-controls': [1018, 45, 262, 44],
  favorites: [0, 92, 574, 29],
  wordmark: [490, 330, 300, 124],
  'home-search': [393, 447, 494, 77],
  'quiet-boundary': [0, 57, 1280, 66],
  menu: [905, 84, 375, 642],
  'menu-top-rows': [918, 88, 350, 112],
  suggestions: [164, 82, 620, 175],
  'internal-page': [150, 126, 980, 365],
  'internal-heading': [146, 155, 329, 90],
  'settings-categories': [146, 258, 199, 213],
  'settings-switches': [1058, 301, 74, 147],
  'list-items': [156, 261, 395, 232],
};

const narrow = {
  caption: [0, 0, 43, 44],
  'window-controls': [531, 0, 109, 44],
  tabs: [45, 3, 405, 39],
  'inactive-tab': [155, 5, 110, 34],
  navigation: [3, 45, 104, 44],
  'address-field': [107, 47, 275, 40],
  'page-controls': [524, 45, 116, 44],
  favorites: [0, 92, 574, 29],
  wordmark: [174, 214, 292, 95],
  'home-search': [73, 295, 494, 73],
  'quiet-boundary': [0, 57, 640, 66],
  menu: [267, 0, 373, 480],
  'menu-top-rows': [278, 8, 350, 119],
  suggestions: [105, 82, 435, 175],
  'internal-page': [8, 125, 624, 334],
  'internal-heading': [8, 141, 315, 68],
  'settings-categories': [8, 216, 619, 62],
  'settings-switches': [554, 312, 77, 137],
  'list-items': [8, 258, 341, 205],
};

const closed = [
  'caption', 'window-controls', 'tabs', 'navigation', 'address-field',
  'page-controls', 'favorites', 'wordmark', 'home-search',
];
const cropsByState = {
  idle: closed,
  'tab-hover': ['tabs', 'inactive-tab'],
  quiet: ['quiet-boundary', 'wordmark', 'home-search'],
  menu: ['menu', 'menu-top-rows'],
  'menu-hover': ['menu', 'menu-top-rows'],
  'menu-dismiss': closed,
  address: ['address-field', 'suggestions'],
  settings: ['internal-page', 'internal-heading', 'settings-categories', 'settings-switches'],
  bookmarks: ['internal-page', 'internal-heading', 'list-items'],
  downloads: ['internal-page', 'internal-heading', 'list-items'],
};

export function comparisonCase(name) {
  const [viewport, state, extra] = name.split('/');
  if (extra !== undefined || !Object.hasOwn(viewports, viewport) || !states.includes(state)) {
    throw new Error(`unknown case ${name}; expected desktop|narrow / ${states.join('|')} (no spaces)`);
  }
  const regions = viewport === 'desktop' ? wide : narrow;
  return {
    viewport,
    state,
    size: viewports[viewport],
    dataScope: state === 'downloads' ? 'design-only-demo-data' :
      ['address', 'bookmarks'].includes(state) ? 'requires-matched-data' : 'ui-state',
    crops: cropsByState[state].map(name => ({
      name,
      rect: regions[name],
      maxMeanRgb: 3,
      maxChangedPercent: name === 'inactive-tab' || name === 'menu-top-rows' ? 4 : 7,
    })),
  };
}
