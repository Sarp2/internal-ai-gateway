import type { StackProps } from 'aws-cdk-lib';
import { Stack } from 'aws-cdk-lib';
import type { IVpc, Vpc } from 'aws-cdk-lib/aws-ec2';
import { Cluster, ContainerInsights } from 'aws-cdk-lib/aws-ecs';
import type { Construct } from 'constructs';

type EcsStackProps = StackProps & {
	vpc: Vpc;
};

export class EcsStack extends Stack {
	public readonly cluster: Cluster;

	public constructor(scope: Construct, id: string, props: EcsStackProps) {
		super(scope, id, props);

		this.cluster = new Cluster(this, 'ProxyCluster', {
			// CDK's concrete Vpc is compatible with IVpc, but exactOptionalPropertyTypes
			// rejects one optional property in the upstream type relationship.
			vpc: props.vpc as IVpc,
			clusterName: 'internal-ai-gateway-proxy',
			enableFargateCapacityProviders: true,
			containerInsightsV2: ContainerInsights.ENHANCED,
		});
	}
}
