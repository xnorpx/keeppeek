module.exports = {
	chromeSelector: '#storybook-root > *',
	diffingEngine: 'pixelmatch',
	skipStories: '^(Foundation|Demos)/',
	fileNameFormatter: ({ configurationName, parameters }) => {
		const scenarioId = parameters?.paper?.scenarioId;
		if (!scenarioId) throw new Error('Paper scenario ID is required for Loki capture');
		return `${configurationName}/${scenarioId}`;
	},
	configurations: {
		'chrome.desktop': {
			target: 'chrome.docker',
			width: 1440,
			height: 900,
			disableAutomaticViewportHeight: false,
			storiesFilter: '^(?!.*Mobile).*$'
		},
		'chrome.mobile': {
			target: 'chrome.docker',
			width: 390,
			height: 844,
			mobile: true,
			disableAutomaticViewportHeight: false,
			storiesFilter: 'Mobile'
		}
	}
};
