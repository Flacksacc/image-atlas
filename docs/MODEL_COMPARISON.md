# Compare OpenCLIP and EmbeddingGemma 2

Use the same catalog and photos for a straightforward comparison. Each model has its own image index, while metadata, thumbnails, previews, and your manual edits are shared. Building Gemma's index preserves OpenCLIP's results and reuses existing scan work wherever possible.

## Install the models

From the activated project environment:

```powershell
python -m pip install -e ".[vision,gemma]"
```

Both models run locally. They prefer CUDA when supported by their PyTorch environment; `--cpu` forces CPU processing. Check CUDA availability in the environment that actually runs the model:

```powershell
python -c "import torch; print(torch.__version__); print('CUDA available:', torch.cuda.is_available())"
```

A CPU-only build of PyTorch reports false even when the computer has an NVIDIA GPU. If you use separate environments, `IMAGE_CLI_VISION_PYTHON` chooses the OpenCLIP worker and `IMAGE_CLI_GEMMA_PYTHON` chooses the Gemma worker.

## Build each index

Replace the catalog name and folder with your own. The folder must belong to the selected catalog:

```powershell
$catalog = "Current"
$photos = "D:\Photos"

image-atlas --database $catalog mine $photos --tags embeddings --embedding-model openclip
image-atlas --database $catalog mine $photos --tags embeddings --embedding-model embeddinggemma2
```

Add `--limit 25` for a small first batch. Repeating the command advances to the next unfinished batch; remove the limit to finish the folder. Add `--retry-errors` to retry failures. Images whose cloud contents are unavailable locally are skipped.

Scanning uses two CPU workers by default. Choose `--workers 4` for a larger pool or `--workers 1` for sequential scanning. The range is 1–8, and this option applies only to scan preparation, not model inference. Cancellation retains committed images; restart the same command to resume.

Progress identifies the active model and actual device. `GPU pending` means the model is loading; `GPU no (CPU)` can mean CPU fallback. ETA covers the current step and updates as processing proceeds. Device-wide VRAM can include other applications.

Check the separate embedding counts:

```powershell
image-atlas --database $catalog status
```

**Embeddings done** counts OpenCLIP work. **Gemma embeddings done** counts Gemma work.

## Compare the results

Run the same description with both models:

```powershell
image-atlas --database $catalog semantic-search "a red car at night" --model openclip
image-atlas --database $catalog semantic-search "a red car at night" --model embeddinggemma2
```

Or use the same reference image:

```powershell
image-atlas --database $catalog image-search "D:\Reference Images\example.jpg" --model openclip
image-atlas --database $catalog image-search "D:\Reference Images\example.jpg" --model embeddinggemma2
```

For a visual comparison, run `image-atlas --database $catalog view` twice. Position the windows side by side, select **OpenCLIP** in one and **EmbeddingGemma 2** in the other, and use the same query and filters. In the browser, select the model and enable **Image content** for descriptive text searches.

Compare the relevance and ordering of the returned pictures. The two models' similarity scores do not share a calibrated scale. An index with fewer completed images also searches a smaller collection, so compare coverage before judging results.

## Separate database deployments

Catalog folders cannot overlap within one registry. Use the normal catalog workflow for separate libraries with different folders. Deployments that compare the same folder using independent databases can use isolated catalog registries; each viewer must be launched with the corresponding `IMAGE_CLI_CONFIG_DIR` and Windows account. Check `image-atlas db list` in the environment used to create the database if it does not appear in the viewer.

For an embedding-model comparison alone, keeping both indexes in one catalog avoids duplicate preview storage and a second fresh inventory.
