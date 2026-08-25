# SPDX-License-Identifier: AGPL-3.0-only

from collections.abc import Mapping, Sequence

import numpy as np
import numpy.typing as npt

class Tensor1D:
    def tolist(self) -> list[float]: ...

class Tensor2D:
    def tolist(self) -> list[list[float]]: ...

class Boxes:
    cls: Tensor1D
    conf: Tensor1D
    xyxy: Tensor2D

class Result:
    boxes: Boxes | None
    names: Mapping[int, str]

class YOLO:
    def __init__(self, model: str) -> None: ...
    def predict(self, source: npt.NDArray[np.uint8], *, verbose: bool) -> Sequence[Result]: ...
