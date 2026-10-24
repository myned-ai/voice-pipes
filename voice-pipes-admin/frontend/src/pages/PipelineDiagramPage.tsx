import { useQuery } from '@tanstack/react-query';
import axios from 'axios';
import { ReactFlow, MiniMap, Controls, Background } from 'reactflow';
import type { Node, Edge } from 'reactflow';
import 'reactflow/dist/style.css';

import { API_BASE } from '../config/api';
import type { ApiEnvelope, PipelineGraph } from '../lib/orchestrator';

const API_URL = API_BASE;

type PipelineData = ApiEnvelope<PipelineGraph>;

const fetchPipeline = async (): Promise<PipelineData> => {
  const { data } = await axios.get<PipelineData>(`${API_URL}/api/v1/pipeline`);
  return data;
};

export function PipelineDiagramPage() {
  const { data: pipeline, isLoading, error } = useQuery<PipelineData>({
    queryKey: ['pipeline'],
    queryFn: fetchPipeline,
  });

  if (isLoading) return <div>Loading pipeline data...</div>;
  if (error) return <div>Error loading pipeline. Is the orchestrator running?</div>;
  if (!pipeline?.data?.nodes) return <div>No pipeline data available.</div>;

  const initialNodes: Node[] = pipeline.data.nodes.map((node, index) => ({
    id: node.id,
    position: { x: index * 250, y: 100 }, // basic horizontal layout
    data: { label: `${node.label} (${node.type})` },
    style: { padding: '10px', borderRadius: '5px', border: '1px solid #333' }
  }));

  const initialEdges: Edge[] = pipeline.data.edges.map((edge, index) => ({
    id: `e${index}`,
    source: edge.from,
    target: edge.to,
    animated: true,
    label: edge.data_type,
  }));

  return (
    <div style={{ height: '80vh', width: '100%', border: '1px solid #ccc', marginTop: '20px' }}>
      <h2 style={{ padding: '10px' }}>Pipeline: {pipeline.data.name}</h2>
      <ReactFlow nodes={initialNodes} edges={initialEdges} fitView fitViewOptions={{ maxZoom: 1 }}>
        <MiniMap />
        <Controls />
        <Background />
      </ReactFlow>
    </div>
  );
}
