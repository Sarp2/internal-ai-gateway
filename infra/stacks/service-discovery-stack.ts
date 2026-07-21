import { Stack, type StackProps } from 'aws-cdk-lib';
import type { IVpc, Vpc } from 'aws-cdk-lib/aws-ec2';
import { PrivateDnsNamespace } from 'aws-cdk-lib/aws-servicediscovery';
import type { Construct } from 'constructs';

type ServiceDiscoveryStackProps = StackProps & {
	vpc: Vpc;
};

export class ServiceDiscoveryStack extends Stack {
	public readonly namespace: PrivateDnsNamespace;

	public constructor(scope: Construct, id: string, props: ServiceDiscoveryStackProps) {
		super(scope, id, props);

		this.namespace = new PrivateDnsNamespace(this, 'IntegrationNamespace', {
			name: 'integration.internal',
			vpc: props.vpc as IVpc,
			description: 'Private service discovery namespace for integration-test services.',
		});
	}
}
