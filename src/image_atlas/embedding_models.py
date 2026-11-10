"""Allowlisted local text/image models and their independent catalog columns."""
from dataclasses import dataclass


@dataclass(frozen=True)
class Model:
    key: str
    name: str
    id: str
    prefix: str = ''

    def column(self, suffix=''):
        return self.prefix + 'embedding' + suffix


OPENCLIP = Model('openclip', 'OpenCLIP', 'open_clip:ViT-B-32:laion2b_s34b_b79k')
GEMMA = Model('embeddinggemma2', 'EmbeddingGemma 2', 'google:embeddinggemma-2:512', 'gemma_')
MODELS = (OPENCLIP, GEMMA)


def get_model(value='openclip'):
    if isinstance(value, Model) and value in MODELS:
        return value
    for model in MODELS:
        if value in (model.key, model.id):
            return model
    raise ValueError('Choose openclip or embeddinggemma2')


def model_sql(sql, model='openclip'):
    """Adapt trusted search SQL only; identifiers never come from user input."""
    import re
    spec = get_model(model)
    return re.sub(r'\bembedding(?:_model|_status|_error)?\b',
                  lambda match: spec.prefix + match[0], sql)
