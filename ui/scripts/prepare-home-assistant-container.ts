import { docker, homeAssistantImage } from '../home-assistant-tests/container';

await docker(['pull', homeAssistantImage], 10 * 60_000);
console.log(`Home Assistant image ready: ${homeAssistantImage}`);
