import { spawn } from 'node:child_process';
import { rm } from 'node:fs/promises';
import { createIntegrationLifecycle } from './integration-lifecycle.mjs';
import { prepareIntegrationOutput } from './integration-output.mjs';

const outputDirectory = '.integration';
const outputsFile = `${outputDirectory}/cdk-outputs.json`;
const integrationStackPattern = 'InternalAiGatewayIntegration*';
const command = process.argv[2];
const pnpm = process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm';
let activeChild;

function run(commandName, args) {
	return new Promise((resolve) => {
		const child = spawn(commandName, args, {
			stdio: 'inherit',
		});
		activeChild = child;

		const finish = (exitCode) => {
			if (activeChild === child) {
				activeChild = undefined;
			}
			resolve(exitCode);
		};

		child.on('error', (error) => {
			console.error(error.message);
			finish(1);
		});
		child.on('exit', (code, childSignal) => {
			if (childSignal) {
				console.error(`${commandName} exited after receiving ${childSignal}`);
				finish(1);
				return;
			}
			finish(code ?? 1);
		});
	});
}

async function deploy() {
	await prepareIntegrationOutput(outputDirectory, outputsFile);
	return run(pnpm, [
		'cdk',
		'deploy',
		integrationStackPattern,
		'--context',
		'integrationTests=true',
		'--require-approval',
		'never',
		'--outputs-file',
		outputsFile,
	]);
}

async function destroy() {
	const exitCode = await run(pnpm, [
		'cdk',
		'destroy',
		integrationStackPattern,
		'--context',
		'integrationTests=true',
		'--force',
	]);
	if (exitCode === 0) {
		await rm(outputDirectory, { recursive: true, force: true });
	}
	return exitCode;
}

const lifecycle = createIntegrationLifecycle({
	deploy,
	destroy,
	exit: (code) => process.exit(code),
	logError: (message) => console.error(message),
	runTests: () => run(pnpm, ['test:integration']),
	stop: (signal) => activeChild?.kill(signal),
});

if (command === 'run') {
	process.once('SIGINT', () => void lifecycle.handleInterruption('SIGINT'));
	process.once('SIGTERM', () => void lifecycle.handleInterruption('SIGTERM'));
}

let exitCode;
switch (command) {
	case 'deploy':
		exitCode = await lifecycle.deploy();
		break;
	case 'destroy':
		exitCode = await lifecycle.destroy();
		break;
	case 'run':
		exitCode = await lifecycle.run();
		break;
	default:
		console.error('Usage: node scripts/integration.mjs <deploy|destroy|run>');
		exitCode = 1;
}

process.exitCode = exitCode;
