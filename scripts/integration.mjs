import { spawn } from 'node:child_process';
import { mkdir, rm } from 'node:fs/promises';

const outputDirectory = '.integration';
const outputsFile = `${outputDirectory}/cdk-outputs.json`;
const integrationStackPattern = 'InternalAiGatewayIntegration*';
const command = process.argv[2];
const pnpm = process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm';

function run(commandName, args) {
	return new Promise((resolve) => {
		const child = spawn(commandName, args, {
			stdio: 'inherit',
		});
		child.on('error', (error) => {
			console.error(error.message);
			resolve(1);
		});
		child.on('exit', (code, signal) => {
			if (signal) {
				console.error(`${commandName} exited after receiving ${signal}`);
				resolve(1);
				return;
			}
			resolve(code ?? 1);
		});
	});
}

async function deploy() {
	await mkdir(outputDirectory, { recursive: true });
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

async function runCompleteLifecycle() {
	const deployExitCode = await deploy();
	if (deployExitCode !== 0) {
		const destroyExitCode = await destroy();
		return deployExitCode || destroyExitCode;
	}

	const testExitCode = await run(pnpm, ['test:integration']);
	const destroyExitCode = await destroy();
	return testExitCode || destroyExitCode;
}

let exitCode;
switch (command) {
	case 'deploy':
		exitCode = await deploy();
		break;
	case 'destroy':
		exitCode = await destroy();
		break;
	case 'run':
		exitCode = await runCompleteLifecycle();
		break;
	default:
		console.error('Usage: node scripts/integration.mjs <deploy|destroy|run>');
		exitCode = 1;
}

process.exitCode = exitCode;
