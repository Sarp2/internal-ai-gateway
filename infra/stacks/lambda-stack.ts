import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { StackProps } from 'aws-cdk-lib';
import { Duration, Stack } from 'aws-cdk-lib';
import type { IFunction } from 'aws-cdk-lib/aws-lambda';
import { Architecture, Runtime, Tracing } from 'aws-cdk-lib/aws-lambda';
import { NodejsFunction, OutputFormat } from 'aws-cdk-lib/aws-lambda-nodejs';
import type { Construct } from 'constructs';

const currentDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = join(currentDirectory, '..', '..');

export class LambdaStack extends Stack {
	public readonly engineerUsageFunction: IFunction;

	public constructor(scope: Construct, id: string, props?: StackProps) {
		super(scope, id, props);

		this.engineerUsageFunction = this.createFunction('EngineerUsageFunction', {
			description: 'Returns usage data for the authenticated engineer.',
			entry: join(repositoryRoot, 'functions', 'engineer', 'usage.ts'),
			environment: {
				FUNCTION_AREA: 'engineer',
			},
			memorySize: 128,
			timeout: Duration.seconds(10),
		});

		// TODO: Add an admin Lambda when the first admin workflow is defined.
	}

	private createFunction(
		id: string,
		props: {
			description: string;
			entry: string;
			environment?: Record<string, string>;
			memorySize: number;
			timeout: Duration;
		},
	): NodejsFunction {
		return new NodejsFunction(this, id, {
			runtime: Runtime.NODEJS_24_X,
			architecture: Architecture.ARM_64,
			description: props.description,
			entry: props.entry,
			handler: 'handler',
			memorySize: props.memorySize,
			timeout: props.timeout,
			tracing: Tracing.ACTIVE,
			environment: {
				...props.environment,
				NODE_OPTIONS: '--enable-source-maps',
			},
			bundling: {
				format: OutputFormat.ESM,
				minify: true,
				sourceMap: true,
				target: 'node24',
			},
		});
	}
}
