import type { StackProps } from 'aws-cdk-lib';
import { RemovalPolicy, Stack } from 'aws-cdk-lib';
import { Secret } from 'aws-cdk-lib/aws-secretsmanager';
import type { Construct } from 'constructs';

export class SecretsStack extends Stack {
	public readonly anthropicApiKeySecret: Secret;
	public readonly openAiApiKeySecret: Secret;

	public constructor(scope: Construct, id: string, props?: StackProps) {
		super(scope, id, props);

		this.anthropicApiKeySecret = new Secret(this, 'AnthropicApiKeySecret', {
			description: 'Anthropic provider API key for the internal AI gateway.',
			removalPolicy: RemovalPolicy.RETAIN,
		});

		this.openAiApiKeySecret = new Secret(this, 'OpenAiApiKeySecret', {
			description: 'OpenAI provider API key for the internal AI gateway.',
			removalPolicy: RemovalPolicy.RETAIN,
		});
	}
}
